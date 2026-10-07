//! The request handler against a live Redis (`CYPHER_TEST_REDIS`); each test
//! skips without it. Fresh random identities, links and inboxes keep runs
//! independent of each other and of leftover state.

use bytes::Bytes;
use cypher_crypto::prekey::SignedPreKey;
use cypher_crypto::{IdentityKeyPair, PrekeyBundle};
use cypher_types::{DeviceId, LinkId, PeerId};
use cypher_wire::{ClientMsg, ErrorCode, Frame, ServerMsg, inbox_id};
use rand::RngCore as _;
use rand::rngs::OsRng;

use crate::handler::{CAPABILITY_ONION, CAPABILITY_PUSH, Handler, Pusher};
use crate::push::{Outcome, Subscription};
use crate::store::Store;

async fn handler(relay_addr: Option<&str>) -> Option<Handler> {
    let url = std::env::var("CYPHER_TEST_REDIS").ok()?;
    Some(Handler {
        store: Store::connect(&url).await.unwrap(),
        onion_public: [7; 32],
        relay_addr: relay_addr.map(str::to_owned),
        push: None,
    })
}

async fn ask(h: &Handler, peer: Option<PeerId>, msg: ClientMsg) -> ServerMsg {
    let reply = h.handle(peer, Frame::new(9, msg).encode()).await;
    let frame = Frame::<ServerMsg>::decode(reply).unwrap();
    assert_eq!(frame.req_id, 9, "replies echo the request id");
    frame.msg
}

fn is_error(msg: &ServerMsg, expected: ErrorCode) -> bool {
    matches!(msg, ServerMsg::Error { code } if *code == expected)
}

/// The bundle a client publishes: everything but the trailing OPK flag.
fn base_bundle(id: &IdentityKeyPair) -> Bytes {
    device_bundle(id, DeviceId::FIRST)
}

/// The bundle one device of `id` publishes.
fn device_bundle(id: &IdentityKeyPair, device: DeviceId) -> Bytes {
    let mut out = Vec::new();
    let spk = SignedPreKey::generate(1, &mut OsRng);
    PrekeyBundle::new(id, device, &spk, None).encode(&mut out);
    out.pop();
    Bytes::from(out)
}

/// The owner reads its (empty) inbox, which opens it for writers.
async fn open_inbox(h: &Handler, secret: [u8; 32]) {
    let ServerMsg::InboxBatch { items, .. } = ask(h, None, ClientMsg::InboxFetch { secret }).await
    else {
        panic!("no batch");
    };
    assert!(items.is_empty());
}

fn random32() -> [u8; 32] {
    let mut bytes = [0; 32];
    OsRng.fill_bytes(&mut bytes);
    bytes
}

#[tokio::test]
async fn keys_are_published_fetched_and_forgeries_rejected() {
    let Some(h) = handler(None).await else { return };
    let (alice, mallory) = (IdentityKeyPair::generate(), IdentityKeyPair::generate());
    let publish = |base| ClientMsg::PublishKeys {
        base,
        opks: vec![(1, random32()), (2, random32())],
        replace_opks: true,
    };
    let acked = ask(&h, Some(alice.peer_id()), publish(base_bundle(&alice))).await;
    assert!(
        matches!(acked, ServerMsg::KeysAck { opks_left: 2 }),
        "{acked:?}"
    );

    let fetched = ask(
        &h,
        Some(mallory.peer_id()),
        ClientMsg::FetchKeys {
            peer: alice.peer_id(),
            device: DeviceId::FIRST,
        },
    )
    .await;
    assert!(
        matches!(fetched, ServerMsg::Keys { opk: Some(_), .. }),
        "{fetched:?}"
    );

    let forged = ask(&h, Some(mallory.peer_id()), publish(base_bundle(&alice))).await;
    assert!(
        is_error(&forged, ErrorCode::BadRequest),
        "someone else's bundle"
    );
    let garbage = ask(
        &h,
        Some(alice.peer_id()),
        publish(Bytes::from(vec![0; cypher_wire::BUNDLE_BASE_LEN])),
    )
    .await;
    assert!(is_error(&garbage, ErrorCode::BadRequest));

    let unknown = ClientMsg::FetchKeys {
        peer: IdentityKeyPair::generate().peer_id(),
        device: DeviceId::FIRST,
    };
    assert!(is_error(
        &ask(&h, Some(alice.peer_id()), unknown).await,
        ErrorCode::NotFound
    ));
}

/// Each device of an identity keeps its own bundle and prekeys: publishing
/// from one leaves the other's alone, and a fetch names the device.
#[tokio::test]
async fn every_device_has_its_own_keys() {
    let Some(h) = handler(None).await else { return };
    let (alice, bob) = (IdentityKeyPair::generate(), IdentityKeyPair::generate());
    let me = Some(alice.peer_id());
    for device in [DeviceId(1), DeviceId(2)] {
        let publish = ClientMsg::PublishKeys {
            base: device_bundle(&alice, device),
            opks: vec![(device.0, random32())],
            replace_opks: true,
        };
        let acked = ask(&h, me, publish).await;
        assert!(
            matches!(acked, ServerMsg::KeysAck { opks_left: 1 }),
            "{acked:?}"
        );
    }
    for device in [DeviceId(1), DeviceId(2)] {
        let fetch = ClientMsg::FetchKeys {
            peer: alice.peer_id(),
            device,
        };
        let ServerMsg::Keys { base, opk } = ask(&h, Some(bob.peer_id()), fetch).await else {
            panic!("no keys for {device}");
        };
        let bundle = PrekeyBundle::decode(&[&base[..], &[0]].concat()).unwrap();
        assert_eq!(
            (bundle.device, opk.map(|(id, _)| id)),
            (device, Some(device.0))
        );
    }
    let missing = ClientMsg::FetchKeys {
        peer: alice.peer_id(),
        device: DeviceId(3),
    };
    assert!(is_error(
        &ask(&h, Some(bob.peer_id()), missing).await,
        ErrorCode::NotFound
    ));
}

#[tokio::test]
async fn links_resolve_to_their_creator() {
    let Some(h) = handler(None).await else { return };
    let host = IdentityKeyPair::generate().peer_id();
    let ServerMsg::LinkCreated { link } = ask(&h, Some(host), ClientMsg::CreateLink).await else {
        panic!("link not created");
    };
    let resolved = ask(&h, Some(PeerId([1; 32])), ClientMsg::ResolveLink { link }).await;
    assert!(matches!(resolved, ServerMsg::LinkResolved { peer } if peer == host));

    let unknown = ClientMsg::ResolveLink {
        link: LinkId::random(&mut OsRng),
    };
    assert!(is_error(
        &ask(&h, Some(host), unknown).await,
        ErrorCode::NotFound
    ));
}

#[tokio::test]
async fn anonymous_inbox_put_fetch_and_ack() {
    let Some(h) = handler(None).await else { return };
    let secret = random32();
    let item = Bytes::from_static(b"sealed item");
    let put = ClientMsg::InboxPut {
        inbox: inbox_id(&secret),
        item: item.clone(),
    };
    assert!(
        is_error(&ask(&h, None, put.clone()).await, ErrorCode::NotFound),
        "an inbox its owner never read takes no writes"
    );
    open_inbox(&h, secret).await;
    assert!(matches!(ask(&h, None, put).await, ServerMsg::Done));

    let ServerMsg::InboxBatch { claim, items } =
        ask(&h, None, ClientMsg::InboxFetch { secret }).await
    else {
        panic!("no batch");
    };
    assert_eq!(items, [item]);
    assert!(matches!(
        ask(&h, None, ClientMsg::InboxAck { secret, claim }).await,
        ServerMsg::Done
    ));

    let ServerMsg::InboxBatch { items, .. } = ask(&h, None, ClientMsg::InboxFetch { secret }).await
    else {
        panic!("no batch");
    };
    assert!(items.is_empty(), "acknowledged items are gone");
}

#[tokio::test]
async fn identity_requests_need_an_authenticated_peer() {
    let Some(h) = handler(None).await else { return };
    let anonymous = [
        ClientMsg::CreateLink,
        ClientMsg::Bootstrap,
        ClientMsg::FetchKeys {
            peer: PeerId([2; 32]),
            device: DeviceId::FIRST,
        },
        ClientMsg::ResolveLink {
            link: LinkId::random(&mut OsRng),
        },
    ];
    for msg in anonymous {
        assert!(is_error(&ask(&h, None, msg).await, ErrorCode::Unauthorized));
    }
    let not_a_request = ask(&h, Some(PeerId([3; 32])), ClientMsg::Ping).await;
    assert!(is_error(&not_a_request, ErrorCode::BadRequest));
}

#[tokio::test]
async fn bootstrap_advertises_the_onion_relay_when_configured() {
    let Some(with_relay) = handler(Some("relay.example:9300")).await else {
        return;
    };
    let peer = Some(PeerId([4; 32]));
    let ServerMsg::BootstrapInfo {
        relay_addr,
        onion_key,
        capabilities,
    } = ask(&with_relay, peer, ClientMsg::Bootstrap).await
    else {
        panic!("no bootstrap");
    };
    assert_eq!(
        (relay_addr.as_str(), onion_key, capabilities),
        ("relay.example:9300", [7; 32], CAPABILITY_ONION)
    );

    let Some(without) = handler(None).await else {
        return;
    };
    let info = ask(&without, peer, ClientMsg::Bootstrap).await;
    assert!(matches!(
        info,
        ServerMsg::BootstrapInfo {
            capabilities: 0,
            ..
        }
    ));
}

#[tokio::test]
async fn undecodable_frames_get_bad_request() {
    let Some(h) = handler(None).await else { return };
    let reply = h.handle(None, Bytes::from_static(&[0xEE, 1, 2])).await;
    let frame = Frame::<ServerMsg>::decode(reply).unwrap();
    assert_eq!(frame.req_id, 0);
    assert!(is_error(&frame.msg, ErrorCode::BadRequest));
}

#[tokio::test]
async fn onion_requests_are_answered_once_and_only_while_fresh() {
    use std::time::{SystemTime, UNIX_EPOCH};
    use x25519_dalek::{PublicKey, StaticSecret};

    let Some(h) = handler(None).await else { return };
    let secret = StaticSecret::random_from_rng(OsRng);
    let public = PublicKey::from(&secret).to_bytes();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let fetch = Frame::new(1, ClientMsg::InboxFetch { secret: random32() }).encode();
    let seal = |at| cypher_crypto::onion::seal_request(&public, &fetch, at, &mut OsRng).unwrap();

    let (fresh, reply) = seal(now);
    let answer = crate::onion::handle(&h, &secret, &fresh)
        .await
        .expect("answered");
    let opened = cypher_crypto::onion::open_response(&reply, &answer).unwrap();
    assert!(matches!(
        Frame::<ServerMsg>::decode(Bytes::from(opened)).unwrap().msg,
        ServerMsg::InboxBatch { .. }
    ));
    assert!(
        crate::onion::handle(&h, &secret, &fresh).await.is_none(),
        "a replay gets no second answer"
    );

    let (stale, _) = seal(now - crate::onion::MAX_SKEW_SECS - 1);
    assert!(crate::onion::handle(&h, &secret, &stale).await.is_none());
}

#[tokio::test]
async fn an_inbox_holds_a_bounded_number_of_bytes_until_read() {
    let Some(h) = handler(None).await else { return };
    let secret = random32();
    open_inbox(&h, secret).await;
    let item = Bytes::from(vec![7u8; cypher_wire::MAX_INBOX_ITEM_LEN]);
    let put = || ClientMsg::InboxPut {
        inbox: inbox_id(&secret),
        item: item.clone(),
    };
    let fit = crate::store::MAX_INBOX_BYTES / item.len();
    for _ in 0..fit {
        assert!(matches!(ask(&h, None, put()).await, ServerMsg::Done));
    }
    assert!(is_error(&ask(&h, None, put()).await, ErrorCode::TooLarge));

    // Reading and acknowledging frees the quota again.
    let ServerMsg::InboxBatch { claim, items } =
        ask(&h, None, ClientMsg::InboxFetch { secret }).await
    else {
        panic!("no batch");
    };
    assert!(!items.is_empty());
    ask(&h, None, ClientMsg::InboxAck { secret, claim }).await;
    assert!(matches!(ask(&h, None, put()).await, ServerMsg::Done));
}

#[tokio::test]
async fn writes_do_not_extend_an_inbox() {
    let Some(h) = handler(None).await else { return };
    let secret = random32();
    open_inbox(&h, secret).await;
    let put = ClientMsg::InboxPut {
        inbox: inbox_id(&secret),
        item: Bytes::from_static(b"x"),
    };
    ask(&h, None, put.clone()).await;
    let first = h.store.ttl(b"i:", &inbox_id(&secret)).await;
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    ask(&h, None, put).await;
    assert!(h.store.ttl(b"i:", &inbox_id(&secret)).await < first);
}

#[tokio::test]
async fn links_are_rate_limited_per_identity() {
    let Some(h) = handler(None).await else { return };
    let host = Some(IdentityKeyPair::generate().peer_id());
    for _ in 0..10 {
        assert!(matches!(
            ask(&h, host, ClientMsg::CreateLink).await,
            ServerMsg::LinkCreated { .. }
        ));
    }
    assert!(is_error(
        &ask(&h, host, ClientMsg::CreateLink).await,
        ErrorCode::RateLimited
    ));
}

#[tokio::test]
async fn one_time_prekeys_cannot_be_drained_or_hoarded() {
    let Some(h) = handler(None).await else { return };
    let victim = IdentityKeyPair::generate();
    let opks = |n: u32| (0..n).map(|i| (i, random32())).collect::<Vec<_>>();
    let publish = |opks, replace_opks| ClientMsg::PublishKeys {
        base: base_bundle(&victim),
        opks,
        replace_opks,
    };
    let me = Some(victim.peer_id());
    ask(&h, me, publish(opks(150), true)).await;
    let ServerMsg::KeysAck { opks_left } = ask(&h, me, publish(opks(150), false)).await else {
        panic!("no ack");
    };
    assert_eq!(usize::from(opks_left), 200, "stored prekeys are capped");

    let mut handed_out = 0;
    for _ in 0..25 {
        // A fresh identity per fetch, as an attacker would use.
        let thief = Some(IdentityKeyPair::generate().peer_id());
        let fetch = ClientMsg::FetchKeys {
            peer: victim.peer_id(),
            device: DeviceId::FIRST,
        };
        let ServerMsg::Keys { opk, .. } = ask(&h, thief, fetch).await else {
            panic!("no keys");
        };
        handed_out += usize::from(opk.is_some());
    }
    assert_eq!(handed_out, 20, "the bundle still comes, without a prekey");
}

type Sent = std::sync::Arc<std::sync::Mutex<Vec<Subscription>>>;

/// A handler whose push service answers `outcome` and records what it got.
async fn push_handler(outcome: Outcome) -> Option<(Handler, Sent)> {
    let mut h = handler(None).await?;
    let sent = Sent::default();
    h.push = Some(Pusher::Record(
        std::sync::Arc::clone(&sent),
        [4; 65],
        outcome,
    ));
    Some((h, sent))
}

/// A device's push subscription for `secret`'s inbox.
fn subscribe(secret: [u8; 32], endpoint: &str) -> ClientMsg {
    use p256::elliptic_curve::sec1::ToEncodedPoint as _;
    let point = p256::SecretKey::random(&mut OsRng)
        .public_key()
        .to_encoded_point(false);
    ClientMsg::PushRegister {
        secret,
        endpoint: endpoint.into(),
        p256dh: point.as_bytes().try_into().unwrap(),
        auth: [1; 16],
    }
}

fn put(inbox: [u8; 32]) -> ClientMsg {
    ClientMsg::InboxPut {
        inbox,
        item: Bytes::from_static(b"sealed"),
    }
}

#[tokio::test]
async fn push_is_registered_anonymously_and_signals_once_per_burst() {
    let Some((h, sent)) = push_handler(Outcome::Sent).await else {
        return;
    };
    assert_eq!(
        ask(&h, None, ClientMsg::PushKey).await,
        ServerMsg::PushKey { key: [4; 65] }
    );
    let ServerMsg::BootstrapInfo { capabilities, .. } =
        ask(&h, Some(PeerId(random32())), ClientMsg::Bootstrap).await
    else {
        panic!("no bootstrap");
    };
    assert_eq!(capabilities & CAPABILITY_PUSH, CAPABILITY_PUSH);

    let secret = random32();
    let inbox = inbox_id(&secret);
    open_inbox(&h, secret).await;
    let register = subscribe(secret, "https://ntfy.sh/up1");
    // Over the session it would tie the push address to an identity.
    let signed_in = ask(&h, Some(PeerId(random32())), register.clone()).await;
    assert!(is_error(&signed_in, ErrorCode::BadRequest));
    for bad in [
        subscribe(secret, "http://ntfy.sh/up1"),
        ClientMsg::PushRegister {
            secret,
            endpoint: "https://ntfy.sh/up1".into(),
            p256dh: [0; 65],
            auth: [1; 16],
        },
    ] {
        assert!(is_error(&ask(&h, None, bad).await, ErrorCode::BadRequest));
    }
    assert_eq!(ask(&h, None, register).await, ServerMsg::Done);

    // Two items in a row wake the device once.
    assert_eq!(ask(&h, None, put(inbox)).await, ServerMsg::Done);
    assert_eq!(ask(&h, None, put(inbox)).await, ServerMsg::Done);
    let got = sent.lock().unwrap().clone();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].endpoint, "https://ntfy.sh/up1");

    // The store keeps the subscription by inbox, nothing else.
    let kept = h.store.push_subscription(&inbox).await.unwrap().unwrap();
    assert_eq!(kept, got[0].to_bytes());
    assert_eq!(
        ask(&h, None, ClientMsg::PushUnregister { secret }).await,
        ServerMsg::Done
    );
    assert_eq!(h.store.push_subscription(&inbox).await.unwrap(), None);
}

#[tokio::test]
async fn a_subscription_the_push_service_dropped_is_forgotten() {
    let Some((h, sent)) = push_handler(Outcome::Gone).await else {
        return;
    };
    let secret = random32();
    let inbox = inbox_id(&secret);
    open_inbox(&h, secret).await;
    assert_eq!(
        ask(&h, None, subscribe(secret, "https://ntfy.sh/up2")).await,
        ServerMsg::Done
    );
    assert_eq!(ask(&h, None, put(inbox)).await, ServerMsg::Done);
    assert_eq!(sent.lock().unwrap().len(), 1);
    assert_eq!(h.store.push_subscription(&inbox).await.unwrap(), None);
}

#[tokio::test]
async fn without_a_vapid_key_push_is_unavailable() {
    let Some(h) = handler(None).await else { return };
    assert!(is_error(
        &ask(&h, None, ClientMsg::PushKey).await,
        ErrorCode::Unavailable
    ));
    let register = subscribe(random32(), "https://ntfy.sh/up3");
    assert!(is_error(
        &ask(&h, None, register).await,
        ErrorCode::Unavailable
    ));
    let ServerMsg::BootstrapInfo { capabilities, .. } =
        ask(&h, Some(PeerId(random32())), ClientMsg::Bootstrap).await
    else {
        panic!("no bootstrap");
    };
    assert_eq!(capabilities & CAPABILITY_PUSH, 0);
}
