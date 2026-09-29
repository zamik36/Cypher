use std::io;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use cypher_crypto::IdentityKeyPair;
use cypher_types::{PeerId, SESSION_AUTH_CONTEXT};
use cypher_wire::{ClientMsg, DeliveryStatus, ErrorCode, Frame, PROTOCOL_VERSION, ServerMsg};
use futures::channel::mpsc;
use futures::{SinkExt, StreamExt};

use crate::bus::mem::MemBus;
use crate::metrics::Metrics;
use crate::session::{Gateway, Limits};

const LIMITS: Limits = Limits {
    frames_per_sec: 10_000,
    bytes_per_sec: 1 << 30,
};

fn gateway(bus: &MemBus) -> Arc<Gateway<MemBus>> {
    Arc::new(Gateway::new(bus.clone(), LIMITS, metrics()))
}

fn metrics() -> Metrics {
    Metrics::register(&cypher_server_kit::metrics::Metrics::new().unwrap()).unwrap()
}

struct Client {
    tx: mpsc::Sender<io::Result<Bytes>>,
    rx: mpsc::Receiver<Bytes>,
    id: IdentityKeyPair,
}

impl Client {
    fn connect(gw: &Arc<Gateway<MemBus>>) -> Self {
        Self::connect_as(gw, IdentityKeyPair::generate())
    }

    fn connect_as(gw: &Arc<Gateway<MemBus>>, id: IdentityKeyPair) -> Self {
        let (tx, stream) = mpsc::channel(64);
        let (sink, rx) = mpsc::channel(4096);
        let sink = sink.sink_map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe));
        tokio::spawn(Arc::clone(gw).handle(stream, sink));
        Self { tx, rx, id }
    }

    fn peer(&self) -> PeerId {
        self.id.peer_id()
    }

    async fn send(&mut self, req_id: u32, msg: ClientMsg) {
        let _ = self.tx.send(Ok(Frame::new(req_id, msg).encode())).await;
    }

    async fn recv(&mut self) -> Option<Frame<ServerMsg>> {
        let raw = tokio::time::timeout(Duration::from_secs(2), self.rx.next())
            .await
            .ok()??;
        Some(Frame::<ServerMsg>::decode(raw).unwrap())
    }

    async fn authenticate(&mut self) {
        let peer = self.peer();
        self.send(
            0,
            ClientMsg::Hello {
                version: PROTOCOL_VERSION,
                peer,
            },
        )
        .await;
        let Some(Frame {
            msg: ServerMsg::Challenge { nonce },
            ..
        }) = self.recv().await
        else {
            panic!("expected challenge");
        };
        let mut signed = SESSION_AUTH_CONTEXT.to_vec();
        signed.extend_from_slice(&nonce);
        let signature = self.id.sign(&signed).to_bytes();
        self.send(0, ClientMsg::Auth { signature }).await;
        assert!(matches!(self.recv().await.unwrap().msg, ServerMsg::Ready));
    }

    async fn closed(&mut self) -> bool {
        matches!(
            tokio::time::timeout(Duration::from_secs(2), async {
                while self.rx.next().await.is_some() {}
            })
            .await,
            Ok(())
        )
    }
}

fn send(to: PeerId, want_ack: bool, body: &'static [u8]) -> ClientMsg {
    ClientMsg::Send {
        to,
        want_ack,
        body: Bytes::from_static(body),
    }
}

#[tokio::test]
async fn frames_before_auth_close_the_connection() {
    let gw = gateway(&MemBus::default());
    let mut c = Client::connect(&gw);
    c.send(1, send(PeerId([9; 32]), true, b"x")).await;
    assert!(c.closed().await);
}

#[tokio::test]
async fn bad_signature_is_rejected() {
    let gw = gateway(&MemBus::default());
    let mut c = Client::connect(&gw);
    let peer = c.peer();
    c.send(
        0,
        ClientMsg::Hello {
            version: PROTOCOL_VERSION,
            peer,
        },
    )
    .await;
    assert!(matches!(
        c.recv().await.unwrap().msg,
        ServerMsg::Challenge { .. }
    ));
    c.send(0, ClientMsg::Auth { signature: [0; 64] }).await;
    assert!(matches!(
        c.recv().await.unwrap().msg,
        ServerMsg::Error {
            code: ErrorCode::Unauthorized
        }
    ));
    assert!(c.closed().await);
}

#[tokio::test]
async fn wrong_protocol_version_is_refused() {
    let gw = gateway(&MemBus::default());
    let mut c = Client::connect(&gw);
    let peer = c.peer();
    c.send(0, ClientMsg::Hello { version: 1, peer }).await;
    assert!(matches!(
        c.recv().await.unwrap().msg,
        ServerMsg::Error {
            code: ErrorCode::BadRequest
        }
    ));
    assert!(c.closed().await);
}

#[tokio::test]
async fn sender_is_stamped_by_the_gateway() {
    let gw = gateway(&MemBus::default());
    let mut a = Client::connect(&gw);
    let mut b = Client::connect(&gw);
    a.authenticate().await;
    b.authenticate().await;

    a.send(7, send(b.peer(), true, b"hello")).await;
    let Frame {
        msg: ServerMsg::SendAck { status },
        req_id,
    } = a.recv().await.unwrap()
    else {
        panic!("expected ack");
    };
    assert_eq!((req_id, status), (7, DeliveryStatus::Delivered));
    match b.recv().await.unwrap().msg {
        ServerMsg::Recv { from, body } => {
            assert_eq!(from, a.peer());
            assert_eq!(&body[..], b"hello");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn offline_peer_is_reported() {
    let gw = gateway(&MemBus::default());
    let mut a = Client::connect(&gw);
    a.authenticate().await;
    a.send(3, send(PeerId([4; 32]), true, b"x")).await;
    assert!(matches!(
        a.recv().await.unwrap().msg,
        ServerMsg::SendAck {
            status: DeliveryStatus::Offline
        }
    ));
}

#[tokio::test]
async fn slow_consumer_gets_busy_instead_of_stalling_the_sender() {
    let gw = gateway(&MemBus::default());
    let mut a = Client::connect(&gw);
    let b_peer = PeerId([8; 32]);
    gw.registry.insert(
        b_peer,
        crate::registry::ConnHandle {
            conn_id: u64::MAX,
            outbox: crate::outbox::outbox().0,
            kick: tokio_util::sync::CancellationToken::new(),
        },
    );
    a.authenticate().await;
    a.send(1, send(b_peer, true, b"x")).await;
    assert!(matches!(
        a.recv().await.unwrap().msg,
        ServerMsg::SendAck {
            status: DeliveryStatus::Busy
        }
    ));
}

#[tokio::test(start_paused = true)]
async fn idle_sessions_close_and_activity_extends_them() {
    let gw = gateway(&MemBus::default());
    let mut a = Client::connect(&gw);
    a.authenticate().await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    a.send(1, ClientMsg::Ping).await;
    assert!(matches!(a.recv().await.unwrap().msg, ServerMsg::Pong));
    tokio::time::sleep(Duration::from_secs(45)).await;
    a.send(2, ClientMsg::Ping).await;
    assert!(
        matches!(a.recv().await.unwrap().msg, ServerMsg::Pong),
        "a frame at 30 s keeps the session past the first 60 s"
    );
    tokio::time::sleep(Duration::from_secs(61)).await;
    assert!(a.closed().await, "a minute without frames closes it");
}

#[tokio::test]
async fn second_login_supersedes_the_first() {
    let gw = gateway(&MemBus::default());
    let seed = cypher_crypto::IdentitySeed([5; 32]);
    let mut first = Client::connect_as(&gw, seed.derive_identity());
    first.authenticate().await;
    let mut second = Client::connect_as(&gw, seed.derive_identity());
    second.authenticate().await;
    assert!(matches!(
        first.recv().await.unwrap().msg,
        ServerMsg::Superseded
    ));
    assert!(first.closed().await);
    assert_eq!(gw.registry.get(&second.peer()).map(|_| ()), Some(()));
}

#[tokio::test]
async fn login_on_another_node_supersedes_the_first() {
    let bus = MemBus::default();
    let (gw1, gw2) = (gateway(&bus), gateway(&bus));
    let seed = cypher_crypto::IdentitySeed([6; 32]);
    let mut first = Client::connect_as(&gw1, seed.derive_identity());
    first.authenticate().await;
    let mut second = Client::connect_as(&gw2, seed.derive_identity());
    second.authenticate().await;
    assert!(matches!(
        first.recv().await.unwrap().msg,
        ServerMsg::Superseded
    ));
    assert!(first.closed().await);

    let mut sender = Client::connect(&gw1);
    sender.authenticate().await;
    sender
        .send(1, send(second.peer(), true, b"still here"))
        .await;
    assert!(
        matches!(second.recv().await.unwrap().msg, ServerMsg::Recv { .. }),
        "the new session did not evict itself"
    );
}

#[tokio::test]
async fn delivery_across_gateway_nodes() {
    let bus = MemBus::default();
    let (gw1, gw2) = (gateway(&bus), gateway(&bus));
    let mut a = Client::connect(&gw1);
    let mut b = Client::connect(&gw2);
    a.authenticate().await;
    b.authenticate().await;
    a.send(9, send(b.peer(), true, b"cross-node")).await;
    assert!(matches!(
        a.recv().await.unwrap().msg,
        ServerMsg::SendAck {
            status: DeliveryStatus::Delivered
        }
    ));
    match b.recv().await.unwrap().msg {
        ServerMsg::Recv { from, body } => {
            assert_eq!((from, &body[..]), (a.peer(), &b"cross-node"[..]));
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn control_requests_are_forwarded_with_the_authenticated_peer() {
    let bus = MemBus::default();
    bus.respond_with(cypher_server_kit::SIG_REQUEST_SUBJECT, |peer, req| {
        let Frame { req_id, .. } = Frame::<ClientMsg>::decode(req).unwrap();
        let link = cypher_types::LinkId::random(&mut rand::rngs::OsRng);
        assert!(peer.is_some());
        Frame::new(req_id, ServerMsg::LinkCreated { link }).encode()
    });
    let gw = gateway(&bus);
    let mut a = Client::connect(&gw);
    a.authenticate().await;
    a.send(42, ClientMsg::CreateLink).await;
    let reply = a.recv().await.unwrap();
    assert_eq!(reply.req_id, 42);
    assert!(matches!(reply.msg, ServerMsg::LinkCreated { .. }));
}

#[tokio::test]
async fn ping_is_answered() {
    let gw = gateway(&MemBus::default());
    let mut a = Client::connect(&gw);
    a.authenticate().await;
    a.send(5, ClientMsg::Ping).await;
    assert_eq!(a.recv().await.unwrap(), Frame::new(5, ServerMsg::Pong));
}
