use std::io;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use cypher_crypto::IdentityKeyPair;
use cypher_types::{Addr, DeviceId, MAX_FRAME_SIZE, PeerId, SESSION_AUTH_CONTEXT};
use cypher_wire::{
    ClientMsg, DeliveryStatus, ErrorCode, Frame, MAX_BODY_LEN, PROTOCOL_VERSION, ServerMsg,
};
use futures::channel::mpsc;
use futures::{SinkExt, StreamExt};

use crate::bus::mem::MemBus;
use crate::bus::{Bus, BusError};
use crate::metrics::Metrics;
use crate::outbox::QUEUE_BYTES;
use crate::session::{
    EVICT_LEN, Gateway, IDLE_TIMEOUT, LIMIT_NOTICE_INTERVAL, Limits, WRITE_TIMEOUT, WRITER_GRACE,
};

const LIMITS: Limits = Limits {
    frames_per_sec: 10_000,
    bytes_per_sec: 1 << 30,
    outbox_bytes: 1 << 30,
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
    fn connect<B: Bus>(gw: &Arc<Gateway<B>>) -> Self {
        Self::connect_as(gw, IdentityKeyPair::generate())
    }

    fn connect_as<B: Bus>(gw: &Arc<Gateway<B>>, id: IdentityKeyPair) -> Self {
        Self::connect_device(gw, id, DeviceId::FIRST)
    }

    fn connect_device<B: Bus>(gw: &Arc<Gateway<B>>, id: IdentityKeyPair, device: DeviceId) -> Self {
        Self::connect_with(gw, id, device, 4096)
    }

    /// `sink_buffer` is how many frames the gateway may write before the
    /// client has to read; 0 behaves like a full TCP window.
    fn connect_with<B: Bus>(
        gw: &Arc<Gateway<B>>,
        id: IdentityKeyPair,
        device: DeviceId,
        sink_buffer: usize,
    ) -> Self {
        let (tx, stream) = mpsc::channel(64);
        let (sink, rx) = mpsc::channel(sink_buffer);
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
    assert_eq!(gw.metrics.delivered_local.get(), 1);
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
            outbox: crate::outbox::outbox(&crate::outbox::Budget::new(usize::MAX)).0,
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
    // Q-423: a refused delivery counts as busy, not as delivered.
    assert_eq!(gw.metrics.delivered_local.get(), 0);
    assert_eq!(gw.metrics.busy.get(), 1);
}

fn send_body(to: Addr, want_ack: bool, body: Bytes) -> ClientMsg {
    ClientMsg::Send {
        to: to.peer,
        device: to.device,
        want_ack,
        body,
    }
}

impl Client {
    /// Sends `body` to `to` and returns the status the gateway acknowledged.
    async fn send_acked(&mut self, req_id: u32, to: Addr, body: Bytes) -> DeliveryStatus {
        self.send(req_id, send_body(to, true, body)).await;
        match self.recv().await.unwrap() {
            Frame {
                req_id: acked,
                msg: ServerMsg::SendAck { status },
            } if acked == req_id => status,
            other => panic!("expected an ack for {req_id}, got {other:?}"),
        }
    }

    /// Waits until the gateway handled everything sent before.
    async fn sync(&mut self) {
        self.send(u32::MAX, ClientMsg::Ping).await;
        assert!(matches!(self.recv().await.unwrap().msg, ServerMsg::Pong));
    }
}

/// S-101: clients that stop reading cannot make the gateway hold more than
/// its outbox budget, however many of them there are; once they leave,
/// their queued bytes return to it.
#[tokio::test]
async fn stalled_readers_cannot_hold_more_than_the_outbox_budget() {
    let budget = 2 * QUEUE_BYTES;
    let limits = Limits {
        outbox_bytes: budget,
        ..LIMITS
    };
    let gw = Arc::new(Gateway::new(MemBus::default(), limits, metrics()));
    let mut stalled = Vec::new();
    for _ in 0..4 {
        let mut c = Client::connect_with(&gw, IdentityKeyPair::generate(), DeviceId::FIRST, 0);
        c.authenticate().await;
        stalled.push(c);
    }
    let mut sender = Client::connect(&gw);
    sender.authenticate().await;
    let body = Bytes::from(vec![0u8; MAX_BODY_LEN]);
    // Each round offers every queue more than its own share; writers take
    // a frame or two out before they block, so a few rounds fill all.
    for _round in 0..3 {
        for to in stalled.iter().map(Client::addr) {
            for _ in 0..QUEUE_BYTES / MAX_BODY_LEN + 2 {
                sender.send(0, send_body(to, false, body.clone())).await;
            }
        }
        sender.sync().await;
        assert!(gw.outbox_used() <= budget);
    }
    let queued: usize = stalled
        .iter()
        .map(|c| gw.registry.get(&c.addr()).unwrap().outbox.queued())
        .sum();
    assert_eq!(queued, gw.outbox_used());
    assert!(
        queued + MAX_FRAME_SIZE > budget,
        "the budget is spent: {queued} of {budget}"
    );

    let mut reader = Client::connect(&gw);
    reader.authenticate().await;
    assert_eq!(
        sender.send_acked(2, reader.addr(), body.clone()).await,
        DeliveryStatus::Busy,
        "past the budget a delivery is refused, not queued"
    );

    drop(stalled);
    tokio::time::timeout(Duration::from_secs(2), async {
        while gw.outbox_used() != 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("closed sessions return their queued bytes");
    assert_eq!(
        sender.send_acked(3, reader.addr(), body).await,
        DeliveryStatus::Delivered
    );
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

/// S-102: a client that stopped reading is released once its session idles
/// out, though its writer is stuck on a frame the client never takes.
#[tokio::test(start_paused = true)]
async fn a_client_that_stops_reading_is_released_after_idling() {
    let gw = gateway(&MemBus::default());
    let mut stalled = Client::connect_with(&gw, IdentityKeyPair::generate(), DeviceId::FIRST, 0);
    stalled.authenticate().await;
    let mut sender = Client::connect(&gw);
    sender.authenticate().await;
    for req_id in 1..=4 {
        sender.send(req_id, send(stalled.addr(), false, b"x")).await;
    }
    sender.sync().await;
    assert_eq!(gw.metrics.connections.get(), 2);

    tokio::time::sleep(IDLE_TIMEOUT + WRITER_GRACE + Duration::from_secs(1)).await;
    assert!(gw.registry.get(&stalled.addr()).is_none());
    assert_eq!(gw.metrics.connections.get(), 0, "both sessions are closed");
}

/// S-102: a client that keeps sending but stops reading never idles out;
/// the stalled write ends its session.
#[tokio::test(start_paused = true)]
async fn a_stalled_write_closes_the_session() {
    let gw = gateway(&MemBus::default());
    let mut stalled = Client::connect_with(&gw, IdentityKeyPair::generate(), DeviceId::FIRST, 0);
    stalled.authenticate().await;
    let pings = WRITE_TIMEOUT.as_secs() / 5 + 2;
    for req_id in 0..u32::try_from(pings).unwrap() {
        stalled.send(req_id, ClientMsg::Ping).await;
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    assert!(
        gw.registry.get(&stalled.addr()).is_none(),
        "unanswered for longer than the write timeout"
    );
    assert_eq!(gw.metrics.connections.get(), 0);
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

/// A bus whose subscriptions take a moment, as NATS round trips do.
#[derive(Clone, Default)]
struct SlowSubscribe(MemBus);

impl Bus for SlowSubscribe {
    type Sub = <MemBus as Bus>::Sub;

    fn request(
        &self,
        subject: String,
        peer: Option<PeerId>,
        payload: Bytes,
        timeout: Duration,
    ) -> impl Future<Output = Result<Bytes, BusError>> + Send {
        self.0.request(subject, peer, payload, timeout)
    }

    fn publish(
        &self,
        subject: String,
        payload: Bytes,
    ) -> impl Future<Output = Result<(), BusError>> + Send {
        self.0.publish(subject, payload)
    }

    fn subscribe(
        &self,
        subject: String,
    ) -> impl Future<Output = Result<Self::Sub, BusError>> + Send {
        let inner = self.0.clone();
        async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            inner.subscribe(subject).await
        }
    }
}

/// S-103: two logins of one device racing on two nodes do not both stay
/// signed in, each getting the device's frames.
#[tokio::test(start_paused = true)]
async fn concurrent_logins_on_two_nodes_do_not_both_survive() {
    let bus = SlowSubscribe::default();
    let gw = || Arc::new(Gateway::new(bus.clone(), LIMITS, metrics()));
    let (gw1, gw2, gw3) = (gw(), gw(), gw());
    let seed = cypher_crypto::IdentitySeed([0x5a; 32]);
    let mut one = Client::connect_as(&gw1, seed.derive_identity());
    let mut two = Client::connect_as(&gw2, seed.derive_identity());
    tokio::join!(one.authenticate(), two.authenticate());

    let mut sender = Client::connect(&gw3);
    sender.authenticate().await;
    sender.send(1, send(one.addr(), false, b"dup")).await;
    let mut delivered = 0;
    let mut superseded = 0;
    for session in [&mut one, &mut two] {
        while let Some(Frame { msg, .. }) = session.recv().await {
            match msg {
                ServerMsg::Recv { .. } => delivered += 1,
                ServerMsg::Superseded => superseded += 1,
                other => panic!("unexpected {other:?}"),
            }
        }
    }
    assert!(superseded >= 1, "at least one login evicted the other");
    assert!(delivered <= 1, "the frame reached {delivered} sessions");
}

/// A bus that takes no publishes, as NATS when its connection is gone.
#[derive(Clone, Default)]
struct NoPublish(MemBus);

impl Bus for NoPublish {
    type Sub = <MemBus as Bus>::Sub;

    fn request(
        &self,
        subject: String,
        peer: Option<PeerId>,
        payload: Bytes,
        timeout: Duration,
    ) -> impl Future<Output = Result<Bytes, BusError>> + Send {
        self.0.request(subject, peer, payload, timeout)
    }

    fn publish(
        &self,
        _subject: String,
        _payload: Bytes,
    ) -> impl Future<Output = Result<(), BusError>> + Send {
        std::future::ready(Err(BusError::Unavailable))
    }

    fn subscribe(
        &self,
        subject: String,
    ) -> impl Future<Output = Result<Self::Sub, BusError>> + Send {
        self.0.subscribe(subject)
    }
}

/// Q-403: publishes the bus refuses (the eviction at sign-in, a relay to
/// another node) are counted instead of vanishing.
#[tokio::test]
async fn failed_publishes_are_counted() {
    let gw = Arc::new(Gateway::new(NoPublish::default(), LIMITS, metrics()));
    let mut a = Client::connect(&gw);
    a.authenticate().await;
    assert_eq!(gw.metrics.publish_failed.get(), 1, "the eviction");
    a.send(1, send(first(PeerId([3; 32])), false, b"x")).await;
    a.sync().await;
    assert_eq!(gw.metrics.publish_failed.get(), 2, "the relay");
}

/// An eviction is told from a relayed frame by its length alone.
#[test]
fn relayed_frames_are_never_mistaken_for_evictions() {
    let shortest = cypher_wire::encode_recv(&PeerId([0; 32]), DeviceId::FIRST, &[]);
    assert!(shortest.len() > EVICT_LEN);
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
            cypher_server_kit::Origin::Gateway,
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

/// Q-408: a frame over the rate limit is answered with `RateLimited`
/// instead of leaving the client to time out, but at most once per
/// interval, so a flood cannot make the gateway answer every frame.
#[tokio::test(start_paused = true)]
async fn frames_over_the_rate_limit_are_answered_once_per_interval() {
    // A burst of two frames; the bucket refills on the wall clock, which
    // barely moves while the paused test clock jumps.
    let limits = Limits {
        frames_per_sec: 1,
        ..LIMITS
    };
    let gw = Arc::new(Gateway::new(MemBus::default(), limits, metrics()));
    let mut a = Client::connect(&gw);
    a.authenticate().await;
    for req_id in 1..=5 {
        a.send(req_id, ClientMsg::Ping).await;
    }
    let limited = |req_id| {
        Frame::new(
            req_id,
            ServerMsg::Error {
                code: ErrorCode::RateLimited,
            },
        )
    };
    assert_eq!(a.recv().await.unwrap(), Frame::new(1, ServerMsg::Pong));
    assert_eq!(a.recv().await.unwrap(), Frame::new(2, ServerMsg::Pong));
    assert_eq!(a.recv().await.unwrap(), limited(3));
    assert_eq!(a.recv().await, None, "4 and 5 are dropped silently");
    assert_eq!(gw.metrics.rate_limited.get(), 3);

    tokio::time::sleep(LIMIT_NOTICE_INTERVAL).await;
    a.send(6, ClientMsg::Ping).await;
    assert_eq!(a.recv().await.unwrap(), limited(6), "a new interval");
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

/// What signaling hands a device's gateway for a sealed delivery, and the
/// status it answers.
async fn deliver_sealed(bus: &MemBus, to: Addr, payload: Vec<u8>) -> Option<u8> {
    let subject = cypher_server_kit::peer_subject(
        &to.peer.to_hex(),
        to.device,
        cypher_server_kit::Origin::Sealed,
    );
    let reply = bus
        .request(subject, None, Bytes::from(payload), Duration::from_secs(2))
        .await
        .ok()?;
    reply.first().copied()
}

fn sealed(token: [u8; 32], item: &[u8]) -> Vec<u8> {
    [token.as_slice(), item].concat()
}

#[tokio::test]
async fn a_sealed_delivery_needs_a_token_the_device_told() {
    use cypher_server_kit::SEALED_REFUSED;
    use cypher_wire::access_hash;

    let bus = MemBus::default();
    let gw = gateway(&bus);
    let mut b = Client::connect(&gw);
    b.authenticate().await;
    let token = [7; 32];
    let delivered = Some(DeliveryStatus::Delivered as u8);
    assert_eq!(
        deliver_sealed(&bus, b.addr(), sealed(token, b"early")).await,
        Some(SEALED_REFUSED),
        "no token told yet"
    );

    let hashes = vec![access_hash(&[1; 32]), access_hash(&token)];
    b.send(5, ClientMsg::SetAccess { hashes }).await;
    assert_eq!(b.recv().await.unwrap(), Frame::new(5, ServerMsg::Done));
    assert_eq!(
        deliver_sealed(&bus, b.addr(), sealed(token, b"sealed item")).await,
        delivered
    );
    assert_eq!(
        b.recv().await.unwrap().msg,
        ServerMsg::RecvSealed {
            item: Bytes::from_static(b"sealed item")
        }
    );

    // A wrong token, an empty message (an eviction on the gateway's own
    // subject) or a short one deliver nothing, and the session stays.
    for refused in [sealed([8; 32], b"forged"), Vec::new(), vec![7; 31]] {
        assert_eq!(
            deliver_sealed(&bus, b.addr(), refused).await,
            Some(SEALED_REFUSED)
        );
    }
    assert_eq!(
        deliver_sealed(&bus, b.addr(), sealed(token, b"still here")).await,
        delivered
    );
    assert!(matches!(
        b.recv().await.unwrap().msg,
        ServerMsg::RecvSealed { .. }
    ));

    // Telling no tokens takes the old ones back.
    b.send(6, ClientMsg::SetAccess { hashes: Vec::new() }).await;
    assert_eq!(b.recv().await.unwrap(), Frame::new(6, ServerMsg::Done));
    assert_eq!(
        deliver_sealed(&bus, b.addr(), sealed(token, b"late")).await,
        Some(SEALED_REFUSED)
    );
}
