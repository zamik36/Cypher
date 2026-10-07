use std::io;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use cypher_crypto::IdentityKeyPair;
use cypher_types::{Addr, DeviceId, PeerId, SESSION_AUTH_CONTEXT};
use cypher_wire::{ClientMsg, DeliveryStatus, ErrorCode, Frame, PROTOCOL_VERSION, ServerMsg};
use futures::channel::mpsc;
use futures::{SinkExt, StreamExt};

use crate::bus::Bus as _;
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
    device: DeviceId,
}

impl Client {
    fn connect(gw: &Arc<Gateway<MemBus>>) -> Self {
        Self::connect_as(gw, IdentityKeyPair::generate())
    }

    fn connect_as(gw: &Arc<Gateway<MemBus>>, id: IdentityKeyPair) -> Self {
        Self::connect_device(gw, id, DeviceId::FIRST)
    }

    fn connect_device(gw: &Arc<Gateway<MemBus>>, id: IdentityKeyPair, device: DeviceId) -> Self {
        let (tx, stream) = mpsc::channel(64);
        let (sink, rx) = mpsc::channel(4096);
        let sink = sink.sink_map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe));
        tokio::spawn(Arc::clone(gw).handle(stream, sink));
        Self { tx, rx, id, device }
    }

    fn peer(&self) -> PeerId {
        self.id.peer_id()
    }

    fn addr(&self) -> Addr {
        Addr::new(self.peer(), self.device)
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
        let (peer, device) = (self.peer(), self.device);
        self.send(
            0,
            ClientMsg::Hello {
                version: PROTOCOL_VERSION,
                peer,
                device,
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
        signed.extend_from_slice(&self.device.0.to_le_bytes());
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

fn send(to: Addr, want_ack: bool, body: &'static [u8]) -> ClientMsg {
    ClientMsg::Send {
        to: to.peer,
        device: to.device,
        want_ack,
        body: Bytes::from_static(body),
    }
}

/// The first device of `peer`.
fn first(peer: PeerId) -> Addr {
    Addr::new(peer, DeviceId::FIRST)
}

#[tokio::test]
async fn frames_before_auth_close_the_connection() {
    let gw = gateway(&MemBus::default());
    let mut c = Client::connect(&gw);
    c.send(1, send(first(PeerId([9; 32])), true, b"x")).await;
    assert!(c.closed().await);
}

/// Claims `peer`, answers the challenge with `signature` and expects to be
/// refused.
async fn assert_auth_refused(peer: PeerId, signature: [u8; 64]) {
    let gw = gateway(&MemBus::default());
    let mut c = Client::connect(&gw);
    c.send(
        0,
        ClientMsg::Hello {
            version: PROTOCOL_VERSION,
            peer,
            device: DeviceId::FIRST,
        },
    )
    .await;
    assert!(matches!(
        c.recv().await.unwrap().msg,
        ServerMsg::Challenge { .. }
    ));
    c.send(0, ClientMsg::Auth { signature }).await;
    assert!(matches!(
        c.recv().await.unwrap().msg,
        ServerMsg::Error {
            code: ErrorCode::Unauthorized
        }
    ));
    assert!(c.closed().await);
}

#[tokio::test]
async fn bad_signature_is_rejected() {
    assert_auth_refused(IdentityKeyPair::generate().peer_id(), [0; 64]).await;
}

/// The identity point with `R = identity, s = 0` would pass plain Ed25519
/// verification for any challenge, letting anyone sign in as that peer.
#[tokio::test]
async fn a_small_order_key_cannot_sign_in() {
    let mut identity_point = [0u8; 32];
    identity_point[0] = 1;
    let mut signature = [0u8; 64];
    signature[0] = 1;
    assert_auth_refused(PeerId(identity_point), signature).await;
}

#[tokio::test]
async fn wrong_protocol_version_is_refused() {
    let gw = gateway(&MemBus::default());
    let mut c = Client::connect(&gw);
    let peer = c.peer();
    let hello = ClientMsg::Hello {
        version: 1,
        peer,
        device: DeviceId::FIRST,
    };
    c.send(0, hello).await;
    assert!(matches!(
        c.recv().await.unwrap().msg,
        ServerMsg::Error {
            code: ErrorCode::UnsupportedVersion
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

    a.send(7, send(b.addr(), true, b"hello")).await;
    let Frame {
        msg: ServerMsg::SendAck { status },
        req_id,
    } = a.recv().await.unwrap()
    else {
        panic!("expected ack");
    };
    assert_eq!((req_id, status), (7, DeliveryStatus::Delivered));
    match b.recv().await.unwrap().msg {
        ServerMsg::Recv { from, device, body } => {
            assert_eq!(Addr::new(from, device), a.addr());
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
    a.send(3, send(first(PeerId([4; 32])), true, b"x")).await;
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
    let b = first(PeerId([8; 32]));
    gw.registry.insert(
        b,
        crate::registry::ConnHandle {
            conn_id: u64::MAX,
            outbox: crate::outbox::outbox().0,
            kick: tokio_util::sync::CancellationToken::new(),
        },
    );
    a.authenticate().await;
    a.send(1, send(b, true, b"x")).await;
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
    assert_eq!(gw.registry.get(&second.addr()).map(|_| ()), Some(()));
}

/// Devices of one identity are separate sessions: signing in on one keeps
/// the others, here and across nodes, and each gets only what is sent to
/// it.
#[tokio::test]
async fn two_devices_of_one_identity_stay_online_together() {
    let bus = MemBus::default();
    let (gw1, gw2) = (gateway(&bus), gateway(&bus));
    let seed = cypher_crypto::IdentitySeed([7; 32]);
    let mut first = Client::connect_device(&gw1, seed.derive_identity(), DeviceId(1));
    first.authenticate().await;
    let mut local = Client::connect_device(&gw1, seed.derive_identity(), DeviceId(2));
    local.authenticate().await;
    let mut remote = Client::connect_device(&gw2, seed.derive_identity(), DeviceId(3));
    remote.authenticate().await;

    let mut sender = Client::connect(&gw1);
    sender.authenticate().await;
    for (req_id, to) in [(1, &mut first), (2, &mut local), (3, &mut remote)] {
        sender.send(req_id, send(to.addr(), true, b"yours")).await;
        assert!(matches!(
            sender.recv().await.unwrap().msg,
            ServerMsg::SendAck {
                status: DeliveryStatus::Delivered
            }
        ));
        assert!(matches!(
            to.recv().await.unwrap().msg,
            ServerMsg::Recv { .. }
        ));
    }
    for device in [&mut first, &mut local, &mut remote] {
        assert!(
            device.recv().await.is_none(),
            "nothing else arrived, no device was superseded"
        );
    }
}

/// A device writes to its siblings (that is how they keep in sync), never
/// to itself.
#[tokio::test]
async fn a_device_can_message_its_sibling() {
    let gw = gateway(&MemBus::default());
    let seed = cypher_crypto::IdentitySeed([8; 32]);
    let mut one = Client::connect_device(&gw, seed.derive_identity(), DeviceId(1));
    one.authenticate().await;
    let mut two = Client::connect_device(&gw, seed.derive_identity(), DeviceId(2));
    two.authenticate().await;

    one.send(1, send(one.addr(), false, b"me")).await;
    one.send(2, send(two.addr(), false, b"sync")).await;
    match two.recv().await.unwrap().msg {
        ServerMsg::Recv { from, device, body } => {
            assert_eq!(
                (Addr::new(from, device), &body[..]),
                (one.addr(), &b"sync"[..])
            );
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(one.recv().await.is_none(), "nothing came back to itself");
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
        .send(1, send(second.addr(), true, b"still here"))
        .await;
    assert!(
        matches!(second.recv().await.unwrap().msg, ServerMsg::Recv { .. }),
        "the new session did not evict itself"
    );
}

/// A remote node that never answers must not let one sender pile up
/// pending deliveries: past the limit the sender is told `Busy` at once.
#[tokio::test]
async fn acknowledged_sends_to_a_stalled_node_are_bounded() {
    let bus = MemBus::default();
    let gw = gateway(&bus);
    let mut a = Client::connect(&gw);
    a.authenticate().await;
    let stalled = IdentityKeyPair::generate().peer_id();
    // Subscribed but never replying: every delivery waits for its timeout.
    let _remote = bus
        .subscribe(cypher_server_kit::peer_subject(
            &stalled.to_hex(),
            DeviceId::FIRST,
        ))
        .await
        .unwrap();
    for req_id in 1..=33 {
        a.send(req_id, send(first(stalled), true, b"x")).await;
    }
    let first = a.recv().await.unwrap();
    assert_eq!(first.req_id, 33, "the one over the limit is answered first");
    assert!(matches!(
        first.msg,
        ServerMsg::SendAck {
            status: DeliveryStatus::Busy
        }
    ));
}

#[tokio::test]
async fn delivery_across_gateway_nodes() {
    let bus = MemBus::default();
    let (gw1, gw2) = (gateway(&bus), gateway(&bus));
    let mut a = Client::connect(&gw1);
    let mut b = Client::connect(&gw2);
    a.authenticate().await;
    b.authenticate().await;
    a.send(9, send(b.addr(), true, b"cross-node")).await;
    assert!(matches!(
        a.recv().await.unwrap().msg,
        ServerMsg::SendAck {
            status: DeliveryStatus::Delivered
        }
    ));
    match b.recv().await.unwrap().msg {
        ServerMsg::Recv { from, body, .. } => {
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

#[test]
fn settings_that_would_serve_no_one_are_refused() {
    let valid = || crate::Config {
        gateway_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: None,
        nats: cypher_server_kit::NatsConfig {
            url: "nats://127.0.0.1:4222".into(),
            user: None,
            password: None,
            token: None,
        },
        tls_cert_path: None,
        tls_key_path: None,
        dev_cert_out: None,
        metrics_addr: "127.0.0.1:0".parse().unwrap(),
        max_connections: 10,
        max_connections_per_ip: 2,
        frames_per_sec: 100,
        bytes_per_sec: 1 << 20,
    };
    valid().validate().unwrap();
    for broken in [
        crate::Config {
            max_connections: 0,
            ..valid()
        },
        crate::Config {
            max_connections_per_ip: 0,
            ..valid()
        },
        crate::Config {
            frames_per_sec: 0,
            ..valid()
        },
        crate::Config {
            bytes_per_sec: 1000,
            ..valid()
        },
    ] {
        assert!(broken.validate().is_err());
    }
}
