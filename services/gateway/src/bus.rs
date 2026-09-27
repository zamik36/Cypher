//! Message bus between gateway nodes and signaling (NATS in production).

use std::future::Future;
use std::time::Duration;

use bytes::Bytes;
use cypher_server_kit::PEER_HEADER;
use cypher_types::PeerId;
use futures::{Stream, StreamExt};

pub struct BusMsg {
    pub payload: Bytes,
    pub reply: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusError {
    NoResponders,
    Timeout,
    Unavailable,
}

pub trait Bus: Send + Sync + 'static {
    type Sub: Stream<Item = BusMsg> + Send + Unpin + 'static;

    fn request(
        &self,
        subject: String,
        peer: Option<PeerId>,
        payload: Bytes,
        timeout: Duration,
    ) -> impl Future<Output = Result<Bytes, BusError>> + Send;

    fn publish(&self, subject: String, payload: Bytes) -> impl Future<Output = ()> + Send;

    fn subscribe(
        &self,
        subject: String,
    ) -> impl Future<Output = Result<Self::Sub, BusError>> + Send;
}

impl Bus for async_nats::Client {
    type Sub = futures::stream::Map<async_nats::Subscriber, fn(async_nats::Message) -> BusMsg>;

    async fn request(
        &self,
        subject: String,
        peer: Option<PeerId>,
        payload: Bytes,
        timeout: Duration,
    ) -> Result<Bytes, BusError> {
        let mut req = async_nats::Request::new()
            .payload(payload)
            .timeout(Some(timeout));
        if let Some(peer) = peer {
            let mut headers = async_nats::HeaderMap::new();
            headers.insert(PEER_HEADER, peer.to_hex().as_str());
            req = req.headers(headers);
        }
        match self.send_request(subject, req).await {
            Ok(msg) => Ok(msg.payload),
            Err(e) => Err(match e.kind() {
                async_nats::RequestErrorKind::NoResponders => BusError::NoResponders,
                async_nats::RequestErrorKind::TimedOut => BusError::Timeout,
                _ => BusError::Unavailable,
            }),
        }
    }

    async fn publish(&self, subject: String, payload: Bytes) {
        let _ = Self::publish(self, subject, payload).await;
    }

    async fn subscribe(&self, subject: String) -> Result<Self::Sub, BusError> {
        fn to_bus_msg(m: async_nats::Message) -> BusMsg {
            BusMsg {
                payload: m.payload,
                reply: m.reply.map(|r| r.to_string()),
            }
        }
        Self::subscribe(self, subject)
            .await
            .map(|s| {
                let convert: fn(async_nats::Message) -> BusMsg = to_bus_msg;
                s.map(convert)
            })
            .map_err(|_| BusError::Unavailable)
    }
}

#[cfg(test)]
pub(crate) mod mem {
    //! In-memory bus: exact-subject pub/sub plus a pluggable signaling stub.

    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use tokio::sync::{mpsc, oneshot};
    use tokio_stream::wrappers::UnboundedReceiverStream;

    use super::*;

    type Responder = Arc<dyn Fn(Option<PeerId>, Bytes) -> Bytes + Send + Sync>;

    #[derive(Clone, Default)]
    pub(crate) struct MemBus {
        inner: Arc<Mutex<Inner>>,
    }

    #[derive(Default)]
    struct Inner {
        subs: HashMap<String, Vec<mpsc::UnboundedSender<BusMsg>>>,
        replies: HashMap<String, oneshot::Sender<Bytes>>,
        responders: HashMap<String, Responder>,
        next_inbox: u64,
    }

    impl MemBus {
        pub(crate) fn respond_with(
            &self,
            subject: &str,
            f: impl Fn(Option<PeerId>, Bytes) -> Bytes + Send + Sync + 'static,
        ) {
            self.inner
                .lock()
                .unwrap()
                .responders
                .insert(subject.to_owned(), Arc::new(f));
        }

        fn deliver(&self, subject: &str, msg: impl Fn() -> BusMsg) -> bool {
            let mut inner = self.inner.lock().unwrap();
            let Some(list) = inner.subs.get_mut(subject) else {
                return false;
            };
            list.retain(|tx| !tx.is_closed());
            for tx in list.iter() {
                let _ = tx.send(msg());
            }
            !list.is_empty()
        }
    }

    impl Bus for MemBus {
        type Sub = UnboundedReceiverStream<BusMsg>;

        async fn request(
            &self,
            subject: String,
            peer: Option<PeerId>,
            payload: Bytes,
            timeout: Duration,
        ) -> Result<Bytes, BusError> {
            let responder = self.inner.lock().unwrap().responders.get(&subject).cloned();
            if let Some(f) = responder {
                return Ok(f(peer, payload));
            }
            let (tx, rx) = oneshot::channel();
            let inbox = {
                let mut inner = self.inner.lock().unwrap();
                inner.next_inbox += 1;
                let inbox = format!("_INBOX.{}", inner.next_inbox);
                inner.replies.insert(inbox.clone(), tx);
                inbox
            };
            if !self.deliver(&subject, || BusMsg {
                payload: payload.clone(),
                reply: Some(inbox.clone()),
            }) {
                return Err(BusError::NoResponders);
            }
            tokio::time::timeout(timeout, rx)
                .await
                .map_err(|_| BusError::Timeout)?
                .map_err(|_| BusError::Unavailable)
        }

        fn publish(&self, subject: String, payload: Bytes) -> impl Future<Output = ()> + Send {
            let waiter = self.inner.lock().unwrap().replies.remove(&subject);
            match waiter {
                Some(tx) => {
                    let _ = tx.send(payload);
                }
                None => {
                    self.deliver(&subject, || BusMsg {
                        payload: payload.clone(),
                        reply: None,
                    });
                }
            }
            std::future::ready(())
        }

        fn subscribe(
            &self,
            subject: String,
        ) -> impl Future<Output = Result<Self::Sub, BusError>> + Send {
            let (tx, rx) = mpsc::unbounded_channel();
            self.inner
                .lock()
                .unwrap()
                .subs
                .entry(subject)
                .or_default()
                .push(tx);
            std::future::ready(Ok(UnboundedReceiverStream::new(rx)))
        }
    }
}
