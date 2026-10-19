//! Message bus between gateway nodes and signaling (NATS in production).

use std::future::Future;
use std::time::Duration;

use bytes::Bytes;
use cypher_server_kit::{Origin, PEER_HEADER};
use cypher_types::PeerId;
use futures::{Stream, StreamExt};

/// The header carrying the nonce of a session that announces itself.
const ANNOUNCER_HEADER: &str = "cy-evict";

pub struct BusMsg {
    pub payload: Bytes,
    pub reply: Option<String>,
    /// Who sent it, by the subject it came on.
    pub origin: Option<Origin>,
    /// On an announcement, the nonce of the session that sent it, if its
    /// gateway names one (earlier releases do not).
    pub announcer: Option<u128>,
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

    /// Hands `payload` to the bus. `Ok` means it was queued, not delivered.
    fn publish(
        &self,
        subject: String,
        payload: Bytes,
    ) -> impl Future<Output = Result<(), BusError>> + Send;

    /// Announces a session signing in: an empty message, as earlier
    /// releases send, with the session's `nonce` beside it.
    fn announce(
        &self,
        subject: String,
        nonce: u128,
    ) -> impl Future<Output = Result<(), BusError>> + Send;

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

    async fn publish(&self, subject: String, payload: Bytes) -> Result<(), BusError> {
        Self::publish(self, subject, payload)
            .await
            .map_err(|_| BusError::Unavailable)
    }

    async fn announce(&self, subject: String, nonce: u128) -> Result<(), BusError> {
        let mut headers = async_nats::HeaderMap::new();
        headers.insert(ANNOUNCER_HEADER, format!("{nonce:032x}").as_str());
        self.publish_with_headers(subject, headers, Bytes::new())
            .await
            .map_err(|_| BusError::Unavailable)
    }

    async fn subscribe(&self, subject: String) -> Result<Self::Sub, BusError> {
        fn to_bus_msg(m: async_nats::Message) -> BusMsg {
            // Only announcements are empty and carry a nonce.
            let announcer = m
                .headers
                .as_ref()
                .filter(|_| m.payload.is_empty())
                .and_then(|h| h.get(ANNOUNCER_HEADER))
                .and_then(|v| u128::from_str_radix(v.as_str(), 16).ok());
            BusMsg {
                origin: Origin::of(&m.subject),
                payload: m.payload,
                reply: m.reply.map(|r| r.to_string()),
                announcer,
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
    //! In-memory bus: pub/sub on exact subjects and on `prefix.*`, plus a
    //! pluggable signaling stub.

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
            let wildcard = subject
                .rsplit_once('.')
                .map(|(parent, _)| format!("{parent}.*"));
            let mut inner = self.inner.lock().unwrap();
            let mut delivered = false;
            for pattern in std::iter::once(subject).chain(wildcard.as_deref()) {
                let Some(list) = inner.subs.get_mut(pattern) else {
                    continue;
                };
                list.retain(|tx| !tx.is_closed());
                for tx in list.iter() {
                    let _ = tx.send(BusMsg {
                        origin: Origin::of(subject),
                        ..msg()
                    });
                    delivered = true;
                }
            }
            delivered
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
                origin: None,
                announcer: None,
            }) {
                return Err(BusError::NoResponders);
            }
            tokio::time::timeout(timeout, rx)
                .await
                .map_err(|_| BusError::Timeout)?
                .map_err(|_| BusError::Unavailable)
        }

        fn publish(
            &self,
            subject: String,
            payload: Bytes,
        ) -> impl Future<Output = Result<(), BusError>> + Send {
            let waiter = self.inner.lock().unwrap().replies.remove(&subject);
            match waiter {
                Some(tx) => {
                    let _ = tx.send(payload);
                }
                None => {
                    self.deliver(&subject, || BusMsg {
                        payload: payload.clone(),
                        reply: None,
                        origin: None,
                        announcer: None,
                    });
                }
            }
            std::future::ready(Ok(()))
        }

        fn announce(
            &self,
            subject: String,
            nonce: u128,
        ) -> impl Future<Output = Result<(), BusError>> + Send {
            self.deliver(&subject, || BusMsg {
                payload: Bytes::new(),
                reply: None,
                origin: None,
                announcer: Some(nonce),
            });
            std::future::ready(Ok(()))
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
