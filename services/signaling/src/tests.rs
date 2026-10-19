//! The request handler against a live Redis (`CYPHER_TEST_REDIS`); each test
//! skips without it locally and fails without it in CI. Fresh random
//! identities, links and inboxes keep runs independent of each other and of
//! leftover state.

use bytes::Bytes;
use cypher_crypto::inbox::{InboxKey, InboxOp, push_extra};
use cypher_crypto::prekey::SignedPreKey;
use cypher_crypto::{DeviceList, IdentityKeyPair, IdentitySeed, PrekeyBundle};
use cypher_types::{DeviceId, LinkId, PeerId};
use cypher_wire::{ClientMsg, DeliveryStatus, ErrorCode, Frame, InboxProof, ServerMsg};
use rand::RngCore as _;
use rand::rngs::OsRng;

use crate::handler::{
    CAPABILITY_ONION, CAPABILITY_PUSH, Caller, Courier, Delivery, Handler, PUSH_PERMITS, Pusher,
    anonymous_allowed,
};
use crate::push::{Outcome, Subscription};
use crate::store::{InboxQuota, Store};

/// A handler on the live Redis, whose inboxes have counters of their own:
/// what other tests and runs leave in their inboxes counts elsewhere.
async fn handler(relay_addr: Option<&str>) -> Option<Handler> {
    handler_with(relay_addr, InboxQuota::default()).await
}

async fn handler_with(relay_addr: Option<&str>, quota: InboxQuota) -> Option<Handler> {
    let url = cypher_server_kit::testing::live_env("CYPHER_TEST_REDIS")?;
    let quota = InboxQuota {
        scope: random32().to_vec(),
        ..quota
    };
    Some(Handler {
        store: Store::connect_with(&url, quota).await.unwrap(),
        signing_key: [7; 32],
        relay_addr: relay_addr.map(str::to_owned),
        push: None,
        courier: Courier::Stub(std::sync::Arc::new(|_, _| Delivery::Away)),
        fetches: tokio::sync::Semaphore::new(crate::handler::FETCH_PERMITS),
        pushes: std::sync::Arc::new(tokio::sync::Semaphore::new(PUSH_PERMITS)),
        push_shed: prometheus::IntCounter::new("push_shed", "test").unwrap(),
        sealed_unavailable: prometheus::IntCounter::new("sealed_unavailable", "test").unwrap(),
        inbox_quota_refused: prometheus::IntCounter::new("inbox_quota_refused", "test").unwrap(),
    })
}

/// Asks `msg` as `peer` signed in at a gateway, or without one as no one in
/// particular, as the onion path does once its filter let `msg` through.
async fn ask(h: &Handler, peer: Option<PeerId>, msg: ClientMsg) -> ServerMsg {
    let caller = peer.map_or(Caller::Onion, Caller::Session);
    let reply = h.handle(caller, Frame::new(9, msg).encode()).await;
    decode_reply(reply)
}

fn decode_reply(reply: Bytes) -> ServerMsg {
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

/// A device's inbox in one epoch, which signs its owner's requests.
struct Inbox(InboxKey);

impl Inbox {
    fn random() -> Self {
        Self(IdentitySeed(random32()).derive_inbox_key(DeviceId::FIRST, 1))
    }

    fn id(&self) -> [u8; 32] {
        self.0.id()
    }

    fn proof_at(&self, op: InboxOp, extra: &[u8], timestamp: u64) -> InboxProof {
        InboxProof {
            inbox: self.id(),
            timestamp,
            signature: self.0.sign_op(op, timestamp, extra),
        }
    }

    fn proof(&self, op: InboxOp, extra: &[u8]) -> InboxProof {
        self.proof_at(op, extra, crate::now_secs())
    }

    fn fetch(&self) -> ClientMsg {
        ClientMsg::InboxFetch {
            proof: self.proof(InboxOp::Fetch, &[]),
            close: false,
        }
    }

    fn close(&self) -> ClientMsg {
        ClientMsg::InboxFetch {
            proof: self.proof(InboxOp::Close, &[]),
            close: true,
        }
    }

    fn ack(&self, claim: [u8; 16]) -> ClientMsg {
        ClientMsg::InboxAck {
            proof: self.proof(InboxOp::Ack, &claim),
            claim,
        }
    }

    fn register(&self, endpoint: &str, p256dh: [u8; 65]) -> ClientMsg {
        let auth = [1; 16];
        ClientMsg::PushRegister {
            proof: self.proof(InboxOp::PushRegister, &push_extra(endpoint, &p256dh, &auth)),
            endpoint: endpoint.into(),
            p256dh,
            auth,
        }
    }

    fn unregister(&self) -> ClientMsg {
        ClientMsg::PushUnregister {
            proof: self.proof(InboxOp::PushUnregister, &[]),
        }
    }
}

/// The owner reads its (empty) inbox, which opens it for writers.
async fn open_inbox(h: &Handler, owner: &Inbox) {
    let ServerMsg::InboxBatch { items, .. } = ask(h, None, owner.fetch()).await else {
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
    let owner = Inbox::random();
    let item = Bytes::from_static(b"sealed item");
    let put = ClientMsg::InboxPut {
        inbox: owner.id(),
        item: item.clone(),
    };
    assert!(
        is_error(&ask(&h, None, put.clone()).await, ErrorCode::NotFound),
        "an inbox its owner never read takes no writes"
    );
    open_inbox(&h, &owner).await;
    assert!(matches!(ask(&h, None, put).await, ServerMsg::Done));

    let ServerMsg::InboxBatch { claim, items } = ask(&h, None, owner.fetch()).await else {
        panic!("no batch");
    };
    assert_eq!(items, [item]);
    assert!(matches!(
        ask(&h, None, owner.ack(claim)).await,
        ServerMsg::Done
    ));

    let ServerMsg::InboxBatch { items, .. } = ask(&h, None, owner.fetch()).await else {
        panic!("no batch");
    };
    assert!(items.is_empty(), "acknowledged items are gone");
}

#[tokio::test]
async fn only_the_owner_reads_an_inbox_and_only_now() {
    let Some(h) = handler(None).await else { return };
    let owner = Inbox::random();
    let now = crate::now_secs();
    let fetch = |proof| ClientMsg::InboxFetch {
        proof,
        close: false,
    };
    let mut forged = owner.proof(InboxOp::Fetch, &[]);
    forged.signature[0] ^= 1;
    let mut elsewhere = Inbox::random().proof(InboxOp::Fetch, &[]);
    elsewhere.inbox = owner.id();
    for refused in [
        fetch(forged),
        fetch(elsewhere),
        fetch(owner.proof_at(InboxOp::Fetch, &[], now - 300)),
        fetch(owner.proof_at(InboxOp::Fetch, &[], now + 300)),
        // A signature for one request does not stand for another.
        fetch(owner.proof(InboxOp::Ack, &[])),
        ClientMsg::InboxFetch {
            proof: owner.proof(InboxOp::Fetch, &[]),
            close: true,
        },
        ClientMsg::InboxAck {
            proof: owner.proof(InboxOp::Ack, &[1; 16]),
            claim: [2; 16],
        },
        ClientMsg::PushUnregister {
            proof: owner.proof(InboxOp::PushRegister, &[]),
        },
    ] {
        assert!(
            is_error(&ask(&h, None, refused).await, ErrorCode::Unauthorized),
            "an unproven request is refused"
        );
    }
    assert!(
        is_error(&ask(&h, None, put(owner.id())).await, ErrorCode::NotFound),
        "no refused fetch opened the inbox"
    );
    open_inbox(&h, &owner).await;
}

#[tokio::test]
async fn a_closed_inbox_takes_no_more_items_but_gives_up_what_it_holds() {
    let Some((h, _)) = push_handler(Outcome::Sent).await else {
        return;
    };
    let owner = Inbox::random();
    open_inbox(&h, &owner).await;
    assert_eq!(
        ask(&h, None, subscribe(&owner, "https://ntfy.sh/up4")).await,
        ServerMsg::Done
    );
    assert_eq!(ask(&h, None, put(owner.id())).await, ServerMsg::Done);

    let ServerMsg::InboxBatch { claim, items } = ask(&h, None, owner.close()).await else {
        panic!("no batch");
    };
    assert_eq!(items.len(), 1);
    assert!(is_error(
        &ask(&h, None, put(owner.id())).await,
        ErrorCode::NotFound
    ));
    assert_eq!(h.store.push_subscription(&owner.id()).await.unwrap(), None);
    assert_eq!(ask(&h, None, owner.ack(claim)).await, ServerMsg::Done);
    let ServerMsg::InboxBatch { items, .. } = ask(&h, None, owner.close()).await else {
        panic!("no batch");
    };
    assert!(items.is_empty());
}

fn publish_devices(list: &DeviceList) -> ClientMsg {
    ClientMsg::PublishDevices {
        list: Bytes::from(list.encode()),
    }
}

fn fetch_devices(peer: PeerId) -> ClientMsg {
    ClientMsg::FetchDevices { peer }
}

/// The server keeps the newest list an identity signed: an equal or older
/// version, as a replayed or rolled-back list would be, is refused.
#[tokio::test]
async fn device_list_only_moves_forward() {
    let Some(h) = handler(None).await else { return };
    let (alice, bob) = (IdentityKeyPair::generate(), IdentityKeyPair::generate());
    let me = Some(alice.peer_id());
    assert!(is_error(
        &ask(&h, Some(bob.peer_id()), fetch_devices(alice.peer_id())).await,
        ErrorCode::NotFound
    ));
    let first = DeviceList::sign(&alice, 1, [DeviceId(1)]).unwrap();
    let second = first.with(&alice, DeviceId(2)).unwrap();
    assert_eq!(ask(&h, me, publish_devices(&first)).await, ServerMsg::Done);
    assert_eq!(ask(&h, me, publish_devices(&second)).await, ServerMsg::Done);
    for stale in [&first, &second] {
        assert!(is_error(
            &ask(&h, me, publish_devices(stale)).await,
            ErrorCode::Conflict
        ));
    }
    let ServerMsg::Devices { list } =
        ask(&h, Some(bob.peer_id()), fetch_devices(alice.peer_id())).await
    else {
        panic!("no list");
    };
    assert_eq!(DeviceList::decode(&list).unwrap(), second);
}

/// Only the identity a list names may publish it, and only a list it
/// signed.
#[tokio::test]
async fn a_device_list_for_another_identity_is_refused() {
    let Some(h) = handler(None).await else { return };
    let (alice, mallory) = (IdentityKeyPair::generate(), IdentityKeyPair::generate());
    let list = DeviceList::sign(&alice, 1, [DeviceId(1)]).unwrap();
    let forged = ask(&h, Some(mallory.peer_id()), publish_devices(&list)).await;
    assert!(is_error(&forged, ErrorCode::BadRequest));
    let mut tampered = list.encode();
    tampered[40] ^= 1;
    let tampered = ClientMsg::PublishDevices {
        list: Bytes::from(tampered),
    };
    assert!(is_error(
        &ask(&h, Some(alice.peer_id()), tampered).await,
        ErrorCode::BadRequest
    ));
}

/// A device dropped from the list loses its keys: nobody starts a session
/// with it any more.
#[tokio::test]
async fn removed_devices_lose_their_keys() {
    let Some(h) = handler(None).await else { return };
    let (alice, bob) = (IdentityKeyPair::generate(), IdentityKeyPair::generate());
    let me = Some(alice.peer_id());
    for device in [DeviceId(1), DeviceId(2)] {
        let publish = ClientMsg::PublishKeys {
            base: device_bundle(&alice, device),
            opks: vec![(1, random32())],
            replace_opks: true,
        };
        ask(&h, me, publish).await;
    }
    let both = DeviceList::sign(&alice, 1, [DeviceId(1), DeviceId(2)]).unwrap();
    ask(&h, me, publish_devices(&both)).await;
    let one = both.without(&alice, DeviceId(2)).unwrap();
    assert_eq!(ask(&h, me, publish_devices(&one)).await, ServerMsg::Done);
    let keys = |device| ClientMsg::FetchKeys {
        peer: alice.peer_id(),
        device,
    };
    let asker = Some(bob.peer_id());
    assert!(matches!(
        ask(&h, asker, keys(DeviceId(1))).await,
        ServerMsg::Keys { .. }
    ));
    assert!(is_error(
        &ask(&h, asker, keys(DeviceId(2))).await,
        ErrorCode::NotFound
    ));
}

#[tokio::test]
async fn fetching_device_lists_is_rate_limited() {
    let Some(h) = handler(None).await else { return };
    let asker = Some(IdentityKeyPair::generate().peer_id());
    let target = IdentityKeyPair::generate().peer_id();
    for _ in 0..120 {
        let answer = ask(&h, asker, fetch_devices(target)).await;
        assert!(is_error(&answer, ErrorCode::NotFound), "{answer:?}");
    }
    assert!(is_error(
        &ask(&h, asker, fetch_devices(target)).await,
        ErrorCode::RateLimited
    ));
}

#[tokio::test]
async fn identity_requests_need_an_authenticated_peer() {
    let Some(h) = handler(None).await else { return };
    let someone = IdentityKeyPair::generate();
    let anonymous = [
        ClientMsg::CreateLink,
        ClientMsg::Bootstrap,
        ClientMsg::PublishKeys {
            base: base_bundle(&someone),
            opks: Vec::new(),
            replace_opks: false,
        },
        ClientMsg::PublishDevices {
            list: Bytes::from_static(b"list"),
        },
    ];
    for msg in anonymous {
        assert!(is_error(&ask(&h, None, msg).await, ErrorCode::Unauthorized));
    }
    let not_a_request = ask(&h, Some(PeerId([3; 32])), ClientMsg::Ping).await;
    assert!(is_error(&not_a_request, ErrorCode::BadRequest));
}

/// Introductions are answered without knowing who asks: the requester's
/// own limits do not apply, the one-time prekey ration per target does.
#[tokio::test]
async fn introductions_are_answered_anonymously() {
    let Some(h) = handler(None).await else { return };
    let host = IdentityKeyPair::generate();
    let me = Some(host.peer_id());
    let opks = (0..30).map(|i| (i, random32())).collect::<Vec<_>>();
    let publish = ClientMsg::PublishKeys {
        base: base_bundle(&host),
        opks,
        replace_opks: true,
    };
    assert!(matches!(
        ask(&h, me, publish).await,
        ServerMsg::KeysAck { .. }
    ));
    let ServerMsg::LinkCreated { link } = ask(&h, me, ClientMsg::CreateLink).await else {
        panic!("link not created");
    };

    let resolved = ask(&h, None, ClientMsg::ResolveLink { link }).await;
    assert!(matches!(resolved, ServerMsg::LinkResolved { peer } if Some(peer) == me));
    assert!(is_error(
        &ask(&h, None, fetch_devices(host.peer_id())).await,
        ErrorCode::NotFound
    ));
    let fetch = || ClientMsg::FetchKeys {
        peer: host.peer_id(),
        device: DeviceId::FIRST,
    };
    // Past the per-identity limit of a session: nothing to count here.
    let mut with_opk = 0;
    for _ in 0..61 {
        match ask(&h, None, fetch()).await {
            ServerMsg::Keys { opk, .. } => with_opk += usize::from(opk.is_some()),
            other => panic!("not answered: {other:?}"),
        }
    }
    assert_eq!(with_opk, 20, "one-time prekeys stay rationed per target");
}

/// Instances agree on each epoch's onion key through Redis, publish the
/// current and the next signed with the long-term key, and forget them.
#[tokio::test]
async fn onion_keys_are_shared_signed_and_rotate() {
    use crate::keys::{EPOCH_SECS, OnionKeys, epoch_at};
    use cypher_crypto::onion_keys::KeySet;

    let Some(h) = handler(None).await else { return };
    // Far in the future, so no other test's keys are in the way.
    let now = (10_000 + u64::from(OsRng.next_u32() % 1_000_000)) * EPOCH_SECS + 3600;
    let seed = random32();
    let (one, two) = (OnionKeys::new(&seed), OnionKeys::new(&seed));
    one.refresh(&h.store, now).await.unwrap();
    two.refresh(&h.store, now).await.unwrap();
    let epoch = epoch_at(now);
    for e in [epoch, epoch + 1] {
        let (a, b) = (one.secret(e).unwrap(), two.secret(e).unwrap());
        assert_eq!(a.as_bytes(), b.as_bytes(), "epoch {e} agreed");
    }
    assert!(one.secret(epoch - 1).is_none(), "nothing made for the past");

    let set = KeySet::verify(&one.key_set(now).unwrap(), &one.signing_key()).unwrap();
    let epochs: Vec<u32> = set.keys().iter().map(|k| k.epoch).collect();
    assert_eq!(epochs, [epoch, epoch + 1]);
    assert_eq!(set.pick(now).unwrap().epoch, epoch);

    // A week on, the old current is the previous, kept a while.
    let later = now + EPOCH_SECS;
    one.refresh(&h.store, later).await.unwrap();
    assert!(one.secret(epoch).is_some());
    assert!(one.secret(epoch + 2).is_some());
    let set = KeySet::verify(&one.key_set(later).unwrap(), &one.signing_key()).unwrap();
    assert_eq!(set.pick(later).unwrap().epoch, epoch + 1);
    KeySet::verify(&one.key_set(later).unwrap(), &random32()).unwrap_err();
}

#[tokio::test]
async fn bootstrap_advertises_the_onion_relay_when_configured() {
    let Some(with_relay) = handler(Some("relay.example:9300")).await else {
        return;
    };
    let peer = Some(PeerId([4; 32]));
    let ServerMsg::BootstrapInfo {
        relay_addr,
        signing_key,
        capabilities,
    } = ask(&with_relay, peer, ClientMsg::Bootstrap).await
    else {
        panic!("no bootstrap");
    };
    assert_eq!(
        (relay_addr.as_str(), signing_key, capabilities),
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
    let reply = h
        .handle(Caller::Onion, Bytes::from_static(&[0xEE, 1, 2]))
        .await;
    let frame = Frame::<ServerMsg>::decode(reply).unwrap();
    assert_eq!(frame.req_id, 0);
    assert!(is_error(&frame.msg, ErrorCode::BadRequest));
}

#[tokio::test]
async fn onion_requests_are_answered_once_and_only_while_fresh() {
    let Some(h) = handler(None).await else { return };
    let (keys, key, now) = onion_keys(&h).await;
    let seal_at = |frame: &[u8], at| {
        cypher_crypto::onion::seal_request(key.epoch, &key.public, frame, at, &mut OsRng).unwrap()
    };
    let fetch = Frame::new(1, Inbox::random().fetch()).encode();
    let seal = |at| seal_at(&fetch, at);

    let (fresh, reply) = seal(now);
    let answer = crate::onion::handle(&h, &keys, &fresh)
        .await
        .expect("answered");
    let opened = cypher_crypto::onion::open_response(&reply, &answer).unwrap();
    assert!(matches!(
        Frame::<ServerMsg>::decode(Bytes::from(opened)).unwrap().msg,
        ServerMsg::InboxBatch { .. }
    ));
    assert!(
        crate::onion::handle(&h, &keys, &fresh).await.is_none(),
        "a replay gets no second answer"
    );

    let (stale, _) = seal(now - crate::onion::MAX_SKEW_SECS - 1);
    assert!(crate::onion::handle(&h, &keys, &stale).await.is_none());

    // A key this server never had, or an epoch it holds no key for.
    let (other, _) =
        cypher_crypto::onion::seal_request(key.epoch, &random32(), &fetch, now, &mut OsRng)
            .unwrap();
    assert!(crate::onion::handle(&h, &keys, &other).await.is_none());
    let (gone, _) =
        cypher_crypto::onion::seal_request(key.epoch + 9, &key.public, &fetch, now, &mut OsRng)
            .unwrap();
    assert!(crate::onion::handle(&h, &keys, &gone).await.is_none());
}

/// This server's onion keys, as a client sees them: the key to seal to now.
async fn onion_keys(
    h: &Handler,
) -> (
    crate::keys::OnionKeys,
    cypher_crypto::onion_keys::EpochKey,
    u64,
) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let keys = crate::keys::OnionKeys::new(&random32());
    keys.refresh(&h.store, now).await.unwrap();
    let set = keys.key_set(now).unwrap();
    let set = cypher_crypto::onion_keys::KeySet::verify(&set, &keys.signing_key()).unwrap();
    let key = *set.pick(now).unwrap();
    (keys, key, now)
}

/// What needs an identity is not taken through the onion at all; what
/// does not is answered there.
#[tokio::test]
async fn onion_requests_needing_an_identity_are_dropped() {
    let Some(h) = handler(None).await else { return };
    let (keys, key, now) = onion_keys(&h).await;
    let seal_at = |frame: &[u8], at| {
        cypher_crypto::onion::seal_request(key.epoch, &key.public, frame, at, &mut OsRng).unwrap()
    };
    let create = Frame::new(2, ClientMsg::CreateLink).encode();
    let (blob, _) = seal_at(&create, now);
    assert!(crate::onion::handle(&h, &keys, &blob).await.is_none());
    let resolve = Frame::new(
        3,
        ClientMsg::ResolveLink {
            link: LinkId::random(&mut OsRng),
        },
    )
    .encode();
    let (blob, reply) = seal_at(&resolve, now);
    let answer = crate::onion::handle(&h, &keys, &blob)
        .await
        .expect("answered");
    let opened = cypher_crypto::onion::open_response(&reply, &answer).unwrap();
    assert!(matches!(
        Frame::<ServerMsg>::decode(Bytes::from(opened)).unwrap().msg,
        ServerMsg::Error {
            code: ErrorCode::NotFound
        }
    ));
}

#[tokio::test]
async fn an_inbox_holds_a_bounded_number_of_bytes_until_read() {
    let Some(h) = handler(None).await else { return };
    let owner = Inbox::random();
    open_inbox(&h, &owner).await;
    let item = Bytes::from(vec![7u8; cypher_wire::MAX_INBOX_ITEM_LEN]);
    let put = || ClientMsg::InboxPut {
        inbox: owner.id(),
        item: item.clone(),
    };
    let fit = crate::store::MAX_INBOX_BYTES / item.len();
    for _ in 0..fit {
        assert!(matches!(ask(&h, None, put()).await, ServerMsg::Done));
    }
    assert!(is_error(&ask(&h, None, put()).await, ErrorCode::TooLarge));

    // Reading and acknowledging frees the quota again.
    let ServerMsg::InboxBatch { claim, items } = ask(&h, None, owner.fetch()).await else {
        panic!("no batch");
    };
    assert!(!items.is_empty());
    ask(&h, None, owner.ack(claim)).await;
    assert!(matches!(ask(&h, None, put()).await, ServerMsg::Done));
}

#[tokio::test]
async fn writes_do_not_extend_an_inbox() {
    let Some(h) = handler(None).await else { return };
    let owner = Inbox::random();
    open_inbox(&h, &owner).await;
    let put = ClientMsg::InboxPut {
        inbox: owner.id(),
        item: Bytes::from_static(b"x"),
    };
    ask(&h, None, put.clone()).await;
    let first = h.store.ttl(b"i:", &owner.id()).await;
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    ask(&h, None, put).await;
    assert!(h.store.ttl(b"i:", &owner.id()).await < first);
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

/// One invite is looked up a bounded number of times, and one identity looks
/// up a bounded number of invites.
#[tokio::test]
async fn invite_lookups_are_rate_limited_per_invite_and_per_identity() {
    let Some(h) = handler(None).await else { return };
    let host = Some(IdentityKeyPair::generate().peer_id());
    let ServerMsg::LinkCreated { link } = ask(&h, host, ClientMsg::CreateLink).await else {
        panic!("link not created");
    };
    let resolve = |link: &LinkId| ClientMsg::ResolveLink { link: link.clone() };
    for _ in 0..20 {
        let asker = Some(IdentityKeyPair::generate().peer_id());
        assert!(matches!(
            ask(&h, asker, resolve(&link)).await,
            ServerMsg::LinkResolved { .. }
        ));
    }
    let late = Some(IdentityKeyPair::generate().peer_id());
    assert!(is_error(
        &ask(&h, late, resolve(&link)).await,
        ErrorCode::RateLimited
    ));

    let busy = Some(IdentityKeyPair::generate().peer_id());
    for _ in 0..60 {
        let unknown = LinkId::random(&mut OsRng);
        assert!(is_error(
            &ask(&h, busy, resolve(&unknown)).await,
            ErrorCode::NotFound
        ));
    }
    let one_more = LinkId::random(&mut OsRng);
    assert!(is_error(
        &ask(&h, busy, resolve(&one_more)).await,
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

/// A device's push subscription for `owner`'s inbox.
fn subscribe(owner: &Inbox, endpoint: &str) -> ClientMsg {
    use p256::elliptic_curve::sec1::ToEncodedPoint as _;
    let point = p256::SecretKey::random(&mut OsRng)
        .public_key()
        .to_encoded_point(false);
    owner.register(endpoint, point.as_bytes().try_into().unwrap())
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

    let owner = Inbox::random();
    let inbox = owner.id();
    open_inbox(&h, &owner).await;
    let register = subscribe(&owner, "https://ntfy.sh/up1");
    // Over the session it would tie the push address to an identity.
    let signed_in = ask(&h, Some(PeerId(random32())), register.clone()).await;
    assert!(is_error(&signed_in, ErrorCode::BadRequest));
    for bad in [
        subscribe(&owner, "http://ntfy.sh/up1"),
        owner.register("https://ntfy.sh/up1", [0; 65]),
    ] {
        assert!(is_error(&ask(&h, None, bad).await, ErrorCode::BadRequest));
    }
    assert_eq!(ask(&h, None, register).await, ServerMsg::Done);

    // Two items in a row wake the device once.
    assert_eq!(ask(&h, None, put(inbox)).await, ServerMsg::Done);
    assert_eq!(ask(&h, None, put(inbox)).await, ServerMsg::Done);
    settle(&h).await;
    let got = sent.lock().unwrap().clone();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].endpoint, "https://ntfy.sh/up1");

    // The store keeps the subscription by inbox, nothing else.
    let kept = h.store.push_subscription(&inbox).await.unwrap().unwrap();
    assert_eq!(kept, got[0].to_bytes());
    assert_eq!(ask(&h, None, owner.unregister()).await, ServerMsg::Done);
    assert_eq!(h.store.push_subscription(&inbox).await.unwrap(), None);
}

#[tokio::test]
async fn a_subscription_the_push_service_dropped_is_forgotten() {
    let Some((h, sent)) = push_handler(Outcome::Gone).await else {
        return;
    };
    let owner = Inbox::random();
    let inbox = owner.id();
    open_inbox(&h, &owner).await;
    assert_eq!(
        ask(&h, None, subscribe(&owner, "https://ntfy.sh/up2")).await,
        ServerMsg::Done
    );
    assert_eq!(ask(&h, None, put(inbox)).await, ServerMsg::Done);
    settle(&h).await;
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
    let register = subscribe(&Inbox::random(), "https://ntfy.sh/up3");
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

type Handed = std::sync::Arc<std::sync::Mutex<Vec<(cypher_types::Addr, Bytes)>>>;

/// A courier that answers what the test sets, and keeps what it was handed.
fn recording_courier(h: &mut Handler) -> (std::sync::Arc<std::sync::Mutex<Delivery>>, Handed) {
    use std::sync::{Arc, Mutex};
    let answer = Arc::new(Mutex::new(Delivery::Away));
    let handed = Handed::default();
    let (a, seen) = (Arc::clone(&answer), Arc::clone(&handed));
    h.courier = Courier::Stub(Arc::new(move |to, payload| {
        seen.lock().unwrap().push((to, payload));
        *a.lock().unwrap()
    }));
    (answer, handed)
}

#[tokio::test]
async fn sealed_sends_reach_the_device_or_its_inbox() {
    let Some(mut h) = handler(None).await else {
        return;
    };
    let (answer, handed) = recording_courier(&mut h);
    let to = cypher_types::Addr::new(PeerId(random32()), DeviceId(2));
    let token = random32();
    let send = |inbox| ClientMsg::SendSealed {
        to: to.peer,
        device: to.device,
        token,
        inbox,
        item: Bytes::from_static(b"sealed"),
    };
    let ack = |status| ServerMsg::SendAck { status };
    let refused = |code| ServerMsg::Error { code };
    let owner = Inbox::random();
    open_inbox(&h, &owner).await;

    let cases = [
        (Delivery::Answered(0), None, ack(DeliveryStatus::Delivered)),
        (Delivery::Answered(2), None, ack(DeliveryStatus::Busy)),
        (
            Delivery::Unavailable,
            Some(owner.id()),
            refused(ErrorCode::Unavailable),
        ),
        (Delivery::Away, None, ack(DeliveryStatus::Offline)),
        (
            Delivery::Away,
            Some(owner.id()),
            ack(DeliveryStatus::Queued),
        ),
        (
            Delivery::Answered(cypher_server_kit::SEALED_REFUSED),
            Some(owner.id()),
            refused(ErrorCode::Unauthorized),
        ),
        (
            Delivery::Away,
            Some(Inbox::random().id()),
            refused(ErrorCode::NotFound),
        ),
    ];
    let count = cases.len();
    for (delivery, inbox, expected) in cases {
        *answer.lock().unwrap() = delivery;
        assert_eq!(ask(&h, None, send(inbox)).await, expected, "{delivery:?}");
    }
    let handed = handed.lock().unwrap().clone();
    assert_eq!(handed.len(), count);
    let payload = [token.as_slice(), b"sealed"].concat();
    assert!(
        handed
            .iter()
            .all(|(addr, p)| *addr == to && p[..] == payload[..])
    );

    // Left for the device while it was away, once.
    let ServerMsg::InboxBatch { items, .. } = ask(&h, None, owner.fetch()).await else {
        panic!("no batch");
    };
    assert_eq!(items, [Bytes::from_static(b"sealed")]);
}

/// Never over the session: the gateway would see who sends.
#[tokio::test]
async fn sealed_sends_go_only_through_the_onion() {
    let Some(mut h) = handler(None).await else {
        return;
    };
    let (_, handed) = recording_courier(&mut h);
    let send = ClientMsg::SendSealed {
        to: PeerId(random32()),
        device: DeviceId::FIRST,
        token: random32(),
        inbox: None,
        item: Bytes::from_static(b"sealed"),
    };
    let signed_in = ask(&h, Some(PeerId(random32())), send).await;
    assert!(is_error(&signed_in, ErrorCode::BadRequest));
    assert!(handed.lock().unwrap().is_empty());
}

/// A subscription that ends on its own stops the service with an error,
/// and while it lasts each pool sheds what it has no permit for.
#[tokio::test]
async fn a_subscription_that_ends_is_an_error() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio_util::sync::CancellationToken;

    let shed = prometheus::IntCounter::new("shed", "test").unwrap();
    let (gate, handled) = (CancellationToken::new(), Arc::new(AtomicUsize::new(0)));
    let (g, done) = (gate.clone(), Arc::clone(&handled));
    let messages = futures::stream::iter([1, 2, 3]);
    let ended = crate::serve("sig.test", messages, 1, &shed, move |_| {
        let (g, done) = (g.clone(), Arc::clone(&done));
        async move {
            g.cancelled().await;
            done.fetch_add(1, Ordering::SeqCst);
        }
    })
    .await;
    assert!(ended.to_string().contains("sig.test"), "{ended}");
    assert_eq!(shed.get(), 2, "one permit: the first is handled, two shed");

    let shutdown = CancellationToken::new();
    crate::stopped(Some(ended), &shutdown).unwrap_err();
    shutdown.cancel();
    let ended = anyhow::anyhow!("closed while shutting down");
    crate::stopped(Some(ended), &shutdown).unwrap();
    crate::stopped(None, &CancellationToken::new()).unwrap();

    gate.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while handled.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

/// `sig.req` is the session path: a request on it without a valid peer
/// header is refused, never answered as anonymous (S-111).
#[tokio::test]
async fn forwarded_requests_without_a_valid_peer_are_refused() {
    let Some(mut h) = handler(None).await else {
        return;
    };
    let (_, handed) = recording_courier(&mut h);
    let owner = Inbox::random();
    let sealed = ClientMsg::SendSealed {
        to: PeerId(random32()),
        device: DeviceId::FIRST,
        token: random32(),
        inbox: None,
        item: Bytes::from_static(b"sealed"),
    };
    let mut short = PeerId(random32()).to_hex();
    short.truncate(62);
    for header in [None, Some(""), Some("zz"), Some(short.as_str())] {
        for msg in [sealed.clone(), owner.fetch(), ClientMsg::PushKey] {
            let reply = h
                .handle_forwarded(header, Frame::new(9, msg).encode())
                .await;
            let reply = decode_reply(reply);
            assert!(
                is_error(&reply, ErrorCode::BadRequest),
                "{header:?}: {reply:?}"
            );
        }
    }
    assert!(handed.lock().unwrap().is_empty(), "nothing was delivered");

    let peer = PeerId(random32()).to_hex();
    let reply = h
        .handle_forwarded(Some(&peer), Frame::new(9, owner.fetch()).encode())
        .await;
    assert!(matches!(decode_reply(reply), ServerMsg::InboxBatch { .. }));
}

/// What the onion lets through: requests that name a blind inbox and
/// introductions, nothing that needs an identity or is not a signaling
/// request at all.
#[test]
fn only_requests_that_need_no_identity_pass_the_onion() {
    let proof = Inbox::random().proof(InboxOp::Fetch, &[]);
    let anonymous = [
        put([1; 32]),
        ClientMsg::InboxFetch {
            proof,
            close: false,
        },
        ClientMsg::InboxAck {
            proof,
            claim: [0; 16],
        },
        ClientMsg::PushKey,
        Inbox::random().register("https://ntfy.sh/x", [4; 65]),
        ClientMsg::PushUnregister { proof },
        ClientMsg::FetchKeys {
            peer: PeerId([1; 32]),
            device: DeviceId::FIRST,
        },
        fetch_devices(PeerId([1; 32])),
        ClientMsg::ResolveLink {
            link: LinkId::random(&mut OsRng),
        },
        ClientMsg::SendSealed {
            to: PeerId([1; 32]),
            device: DeviceId::FIRST,
            token: [0; 32],
            inbox: None,
            item: Bytes::new(),
        },
    ];
    let identified = [
        ClientMsg::PublishKeys {
            base: Bytes::new(),
            opks: Vec::new(),
            replace_opks: false,
        },
        ClientMsg::PublishDevices { list: Bytes::new() },
        ClientMsg::CreateLink,
        ClientMsg::Bootstrap,
        ClientMsg::Hello {
            version: 4,
            peer: PeerId([1; 32]),
            device: DeviceId::FIRST,
        },
        ClientMsg::Auth { signature: [0; 64] },
        ClientMsg::Ping,
        ClientMsg::Send {
            to: PeerId([1; 32]),
            device: DeviceId::FIRST,
            want_ack: false,
            body: Bytes::new(),
        },
        ClientMsg::SetAccess { hashes: Vec::new() },
    ];
    for msg in &anonymous {
        assert!(anonymous_allowed(msg), "{msg:?}");
    }
    for msg in &identified {
        assert!(!anonymous_allowed(msg), "{msg:?}");
    }
}

fn publish_keys_of(id: &IdentityKeyPair, device: DeviceId) -> ClientMsg {
    ClientMsg::PublishKeys {
        base: device_bundle(id, device),
        opks: vec![(1, random32())],
        replace_opks: true,
    }
}

/// Whether anyone can fetch the keys of `alice`'s `device`.
async fn has_keys(h: &Handler, alice: &IdentityKeyPair, device: DeviceId) -> bool {
    let fetch = ClientMsg::FetchKeys {
        peer: alice.peer_id(),
        device,
    };
    let bob = Some(IdentityKeyPair::generate().peer_id());
    match ask(h, bob, fetch).await {
        ServerMsg::Keys { .. } => true,
        other => {
            assert!(is_error(&other, ErrorCode::NotFound), "{other:?}");
            false
        }
    }
}

/// Only devices an identity lists hold keys, and the two latest of those
/// about to be listed: one identity cannot store keys for any number of
/// devices, and a device that publishes is never refused for it.
#[tokio::test]
async fn keys_are_kept_only_for_listed_devices_and_the_latest_joining() {
    let Some(h) = handler(None).await else { return };
    let alice = IdentityKeyPair::generate();
    let me = Some(alice.peer_id());
    for device in [1000, 1001, 1002, 1000, 1003].map(DeviceId) {
        let answer = ask(&h, me, publish_keys_of(&alice, device)).await;
        assert!(matches!(answer, ServerMsg::KeysAck { .. }), "{answer:?}");
    }
    // 1001 published longest ago and gave way to 1003, then 1002 did; 1000
    // published again in between, so it stays.
    let mut held = Vec::new();
    for device in [1000, 1001, 1002, 1003].map(DeviceId) {
        if has_keys(&h, &alice, device).await {
            held.push(device);
        }
    }
    assert_eq!(held, [DeviceId(1000), DeviceId(1003)]);

    // The list names one of them: the other loses its keys, and listed
    // devices never make others give way.
    let list = DeviceList::sign(&alice, 1, [DeviceId::FIRST, DeviceId(1003)]).unwrap();
    assert_eq!(ask(&h, me, publish_devices(&list)).await, ServerMsg::Done);
    assert!(!has_keys(&h, &alice, DeviceId(1000)).await);
    for device in [
        DeviceId::FIRST,
        DeviceId(1003),
        DeviceId(1004),
        DeviceId(1005),
    ] {
        let answer = ask(&h, me, publish_keys_of(&alice, device)).await;
        assert!(matches!(answer, ServerMsg::KeysAck { .. }), "{answer:?}");
    }
    for device in [
        DeviceId::FIRST,
        DeviceId(1003),
        DeviceId(1004),
        DeviceId(1005),
    ] {
        assert!(has_keys(&h, &alice, device).await, "{device}");
    }
}

/// Key publishes are limited per identity, however many device ids it
/// makes up: 60 an hour for each device it may have.
#[tokio::test]
async fn publishing_keys_is_rate_limited_per_identity() {
    let Some(h) = handler(None).await else { return };
    let alice = IdentityKeyPair::generate();
    let me = Some(alice.peer_id());
    let per_hour = 60 * u32::try_from(cypher_types::MAX_DEVICES).unwrap();
    for device in (1..=per_hour).map(DeviceId) {
        let answer = ask(&h, me, publish_keys_of(&alice, device)).await;
        assert!(matches!(answer, ServerMsg::KeysAck { .. }), "{answer:?}");
    }
    assert!(is_error(
        &ask(&h, me, publish_keys_of(&alice, DeviceId::FIRST)).await,
        ErrorCode::RateLimited
    ));
}

/// Reads and acknowledges everything `owner` holds.
async fn drain(h: &Handler, owner: &Inbox) {
    loop {
        let ServerMsg::InboxBatch { claim, items } = ask(h, None, owner.fetch()).await else {
            panic!("no batch");
        };
        if items.is_empty() {
            return;
        }
        assert_eq!(ask(h, None, owner.ack(claim)).await, ServerMsg::Done);
    }
}

/// What the day counters of `h` count together, as seen at `now`.
async fn budget_held(h: &Handler, now: u64) -> u64 {
    let held: i64 = h.store.inbox_counters(now).await.iter().map(|c| c.0).sum();
    held.unsigned_abs()
}

/// All inboxes together hold a bounded number of bytes, however many
/// anonymous keys open them; reading gives the room back, and the counters
/// expire with what they count.
#[tokio::test]
async fn all_inboxes_together_hold_a_bounded_number_of_bytes() {
    use crate::store::{INBOX_ITEM_OVERHEAD, INBOX_OPENING_COST};
    let item = Bytes::from(vec![7u8; 1000]);
    let cost = item.len() as u64 + INBOX_ITEM_OVERHEAD;
    let opened = 4 * INBOX_OPENING_COST;
    let quota = InboxQuota {
        budget: opened + 10 * cost,
        ..InboxQuota::default()
    };
    let Some(h) = handler_with(None, quota).await else {
        return;
    };
    let owners: Vec<Inbox> = (0..4).map(|_| Inbox::random()).collect();
    for owner in &owners {
        open_inbox(&h, owner).await;
    }
    let put = |owner: &Inbox| ClientMsg::InboxPut {
        inbox: owner.id(),
        item: item.clone(),
    };
    let mut stored = 0;
    for owner in owners.iter().cycle().take(12) {
        match ask(&h, None, put(owner)).await {
            ServerMsg::Done => stored += 1,
            refused => assert!(is_error(&refused, ErrorCode::TooLarge), "{refused:?}"),
        }
    }
    assert_eq!(stored, 10, "the budget, spread over any number of inboxes");
    assert_eq!(h.inbox_quota_refused.get(), 2);
    let now = crate::now_secs();
    assert_eq!(budget_held(&h, now).await, opened + 10 * cost);
    let counters = h.store.inbox_counters(now).await;
    assert!(
        counters
            .iter()
            .all(|&(_, ttl)| ttl > 0 && ttl <= 15 * 86_400),
        "counters end the day their inboxes do: {counters:?}"
    );

    drain(&h, &owners[0]).await;
    assert_eq!(ask(&h, None, put(&owners[1])).await, ServerMsg::Done);
    for owner in &owners {
        drain(&h, owner).await;
    }
    assert_eq!(budget_held(&h, now).await, opened, "only the openings");
    for owner in &owners {
        assert!(matches!(
            ask(&h, None, owner.close()).await,
            ServerMsg::InboxBatch { .. }
        ));
    }
    assert_eq!(h.store.inbox_counters(now).await, [], "nothing gone counts");
}

/// Opening an inbox draws on the budget of all inboxes: past it nothing
/// new opens, while inboxes open already stay open; closing one makes
/// room again.
#[tokio::test]
async fn openings_count_against_the_budget_of_all_inboxes() {
    let quota = InboxQuota {
        budget: 2 * crate::store::INBOX_OPENING_COST,
        ..InboxQuota::default()
    };
    let Some(h) = handler_with(None, quota).await else {
        return;
    };
    let (a, b, c) = (Inbox::random(), Inbox::random(), Inbox::random());
    open_inbox(&h, &a).await;
    open_inbox(&h, &b).await;
    let refused = ask(&h, None, c.fetch()).await;
    assert!(is_error(&refused, ErrorCode::Unavailable), "{refused:?}");
    assert!(
        is_error(&ask(&h, None, put(c.id())).await, ErrorCode::NotFound),
        "the refused inbox is not open to writers"
    );
    assert_eq!(h.inbox_quota_refused.get(), 1);

    open_inbox(&h, &a).await;
    assert!(matches!(
        ask(&h, None, b.close()).await,
        ServerMsg::InboxBatch { .. }
    ));
    open_inbox(&h, &c).await;
}

/// An item put just before midnight and read just after is taken off the
/// counter it was put on: put and ack agree on the day by the list's
/// expiry, not by when they run.
#[tokio::test]
async fn put_and_ack_agree_across_a_day_boundary() {
    let Some(h) = handler(None).await else { return };
    let owner = Inbox::random();
    open_inbox(&h, &owner).await;
    let (before, after) = {
        let day = crate::now_secs() / 86_400;
        (day * 86_400 + 86_399, (day + 1) * 86_400)
    };
    let opened = crate::store::INBOX_OPENING_COST;
    let stored = h.store.inbox_put(&owner.id(), b"sealed", before).await;
    assert!(matches!(stored, Ok(crate::store::PutOutcome::Stored)));
    assert!(budget_held(&h, before).await > opened);

    let ServerMsg::InboxBatch { claim, items } = ask(&h, None, owner.fetch()).await else {
        panic!("no batch");
    };
    assert_eq!(items.len(), 1);
    h.store.inbox_ack(&owner.id(), &claim, after).await.unwrap();
    assert_eq!(budget_held(&h, after).await, opened);
}

/// Over a session, one identity's inbox requests are limited; through the
/// onion the relay limits each asker instead.
#[tokio::test]
async fn inbox_requests_over_a_session_are_rate_limited_per_identity() {
    let Some(h) = handler(None).await else { return };
    let me = Some(PeerId(random32()));
    let owner = Inbox::random();
    for _ in 0..600 {
        let answer = ask(&h, me, owner.fetch()).await;
        assert!(matches!(answer, ServerMsg::InboxBatch { .. }), "{answer:?}");
    }
    let limited = ask(&h, me, owner.fetch()).await;
    assert!(is_error(&limited, ErrorCode::RateLimited), "{limited:?}");
    assert!(matches!(
        ask(&h, None, owner.fetch()).await,
        ServerMsg::InboxBatch { .. }
    ));

    for _ in 0..1200 {
        let answer = ask(&h, me, put(Inbox::random().id())).await;
        assert!(is_error(&answer, ErrorCode::NotFound), "{answer:?}");
    }
    let limited = ask(&h, me, put(owner.id())).await;
    assert!(is_error(&limited, ErrorCode::RateLimited), "{limited:?}");
    assert_eq!(ask(&h, None, put(owner.id())).await, ServerMsg::Done);
}

/// Waits until every push signal on its way got its answer: each holds a
/// permit until then.
async fn settle(h: &Handler) {
    let all = u32::try_from(PUSH_PERMITS).unwrap();
    let wait = h.pushes.acquire_many(all);
    let permits = tokio::time::timeout(std::time::Duration::from_secs(5), wait)
        .await
        .expect("push signals settle")
        .unwrap();
    drop(permits);
}

/// Push signals go out from tasks of their own, a bounded number at once:
/// one past the limit is dropped, not queued, and does not use up the
/// inbox's signal interval (S-110).
#[tokio::test]
async fn push_signals_on_their_way_are_bounded() {
    let Some((h, sent)) = push_handler(Outcome::Sent).await else {
        return;
    };
    let owner = Inbox::random();
    open_inbox(&h, &owner).await;
    assert_eq!(
        ask(&h, None, subscribe(&owner, "https://ntfy.sh/up5")).await,
        ServerMsg::Done
    );
    let all = u32::try_from(PUSH_PERMITS).unwrap();
    let busy = std::sync::Arc::clone(&h.pushes)
        .try_acquire_many_owned(all)
        .unwrap();
    assert_eq!(ask(&h, None, put(owner.id())).await, ServerMsg::Done);
    tokio::task::yield_now().await;
    assert!(sent.lock().unwrap().is_empty(), "dropped while saturated");
    assert_eq!(h.push_shed.get(), 1);

    drop(busy);
    assert_eq!(ask(&h, None, put(owner.id())).await, ServerMsg::Done);
    settle(&h).await;
    assert_eq!(sent.lock().unwrap().len(), 1, "sent once there is room");
}
