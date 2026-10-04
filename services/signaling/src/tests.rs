//! The request handler against a live Redis (`CYPHER_TEST_REDIS`); each test
//! skips without it. Fresh random identities, links and inboxes keep runs
//! independent of each other and of leftover state.

use bytes::Bytes;
use cypher_crypto::prekey::SignedPreKey;
use cypher_crypto::{IdentityKeyPair, PrekeyBundle};
use cypher_types::{LinkId, PeerId};
use cypher_wire::{ClientMsg, ErrorCode, Frame, ServerMsg, inbox_id};
use rand::RngCore as _;
use rand::rngs::OsRng;

use crate::handler::{CAPABILITY_ONION, Handler};
use crate::store::Store;

async fn handler(relay_addr: Option<&str>) -> Option<Handler> {
    let url = std::env::var("CYPHER_TEST_REDIS").ok()?;
    Some(Handler {
        store: Store::connect(&url).await.unwrap(),
        onion_public: [7; 32],
        relay_addr: relay_addr.map(str::to_owned),
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
    let mut out = Vec::new();
    PrekeyBundle::new(id, &SignedPreKey::generate(1, &mut OsRng), None).encode(&mut out);
    out.pop();
    Bytes::from(out)
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
    };
    assert!(is_error(
        &ask(&h, Some(alice.peer_id()), unknown).await,
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
    let put = Frame::new(
        1,
        ClientMsg::InboxPut {
            inbox: random32(),
            item: Bytes::from_static(b"sealed"),
        },
    )
    .encode();
    let seal = |at| cypher_crypto::onion::seal_request(&public, &put, at, &mut OsRng).unwrap();

    let (fresh, reply) = seal(now);
    let answer = crate::onion::handle(&h, &secret, &fresh)
        .await
        .expect("answered");
    let opened = cypher_crypto::onion::open_response(&reply, &answer).unwrap();
    assert!(matches!(
        Frame::<ServerMsg>::decode(Bytes::from(opened)).unwrap().msg,
        ServerMsg::Done
    ));
    assert!(
        crate::onion::handle(&h, &secret, &fresh).await.is_none(),
        "a replay gets no second answer"
    );

    let (stale, _) = seal(now - crate::onion::MAX_SKEW_SECS - 1);
    assert!(crate::onion::handle(&h, &secret, &stale).await.is_none());
}
