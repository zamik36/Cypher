#![expect(
    clippy::unwrap_used,
    reason = "test helpers fail loudly on broken fixtures"
)]

use bytes::Bytes;
use cypher_types::{DeviceId, LinkId, PeerId};
use cypher_wire::{
    BUNDLE_BASE_LEN, ClientMsg, DeliveryStatus, ErrorCode, Frame, MAX_INBOX_ITEM_LEN,
    SEND_HEADER_LEN, SendView, ServerMsg, WireError, encode_recv, peek_send,
};
use proptest::prelude::*;

/// Proptest settings; under Miri a handful of cases and no failure files,
/// which Miri's isolation forbids. Coverage of inputs is the fuzzers' job.
fn proptest_config(cases: u32) -> ProptestConfig {
    if cfg!(miri) {
        ProptestConfig {
            cases: 4,
            failure_persistence: None,
            ..ProptestConfig::default()
        }
    } else {
        ProptestConfig::with_cases(cases)
    }
}

fn link() -> LinkId {
    LinkId::parse("abcdefghijklmnopqrstuvwxyz").unwrap()
}

fn client_samples() -> Vec<ClientMsg> {
    vec![
        ClientMsg::Hello {
            version: 3,
            peer: PeerId([1; 32]),
            device: DeviceId(7),
        },
        ClientMsg::Auth { signature: [2; 64] },
        ClientMsg::Ping,
        ClientMsg::Send {
            to: PeerId([3; 32]),
            device: DeviceId::FIRST,
            want_ack: true,
            body: Bytes::from_static(b"opaque"),
        },
        ClientMsg::Send {
            to: PeerId([3; 32]),
            device: DeviceId(u32::MAX),
            want_ack: false,
            body: Bytes::new(),
        },
        ClientMsg::PublishKeys {
            base: Bytes::from(vec![4; BUNDLE_BASE_LEN]),
            opks: vec![(1, [5; 32]), (2, [6; 32])],
            replace_opks: true,
        },
        ClientMsg::FetchKeys {
            peer: PeerId([7; 32]),
            device: DeviceId(2),
        },
        ClientMsg::CreateLink,
        ClientMsg::ResolveLink { link: link() },
        ClientMsg::InboxPut {
            inbox: [8; 32],
            item: Bytes::from_static(b"sealed"),
        },
        ClientMsg::InboxFetch { secret: [9; 32] },
        ClientMsg::InboxAck {
            secret: [10; 32],
            claim: [11; 16],
        },
        ClientMsg::Bootstrap,
        ClientMsg::PushKey,
        ClientMsg::PushRegister {
            secret: [12; 32],
            endpoint: "https://ntfy.sh/upAbC?up=1".into(),
            p256dh: [4; 65],
            auth: [13; 16],
        },
        ClientMsg::PushUnregister { secret: [14; 32] },
    ]
}

fn server_samples() -> Vec<ServerMsg> {
    vec![
        ServerMsg::Challenge { nonce: [1; 32] },
        ServerMsg::Ready,
        ServerMsg::Pong,
        ServerMsg::Recv {
            from: PeerId([2; 32]),
            device: DeviceId(3),
            body: Bytes::from_static(b"payload"),
        },
        ServerMsg::SendAck {
            status: DeliveryStatus::Offline,
        },
        ServerMsg::Keys {
            base: Bytes::from(vec![3; BUNDLE_BASE_LEN]),
            opk: Some((9, [4; 32])),
        },
        ServerMsg::Keys {
            base: Bytes::from(vec![3; BUNDLE_BASE_LEN]),
            opk: None,
        },
        ServerMsg::KeysAck { opks_left: 17 },
        ServerMsg::LinkCreated { link: link() },
        ServerMsg::LinkResolved {
            peer: PeerId([5; 32]),
        },
        ServerMsg::InboxBatch {
            claim: [6; 16],
            items: vec![
                Bytes::from_static(b"a"),
                Bytes::new(),
                Bytes::from_static(b"ccc"),
            ],
        },
        ServerMsg::Done,
        ServerMsg::BootstrapInfo {
            relay_addr: "relay.example:9443".into(),
            onion_key: [7; 32],
            capabilities: 3,
        },
        ServerMsg::PushKey { key: [4; 65] },
        ServerMsg::Error {
            code: ErrorCode::NotFound,
        },
        ServerMsg::Superseded,
    ]
}

#[test]
fn every_client_message_roundtrips() {
    for (req_id, msg) in (1..).zip(client_samples()) {
        let frame = Frame::new(req_id, msg);
        assert_eq!(Frame::<ClientMsg>::decode(frame.encode()).unwrap(), frame);
    }
}

#[test]
fn every_server_message_roundtrips() {
    for (req_id, msg) in (0..).zip(server_samples()) {
        let frame = Frame::new(req_id, msg);
        assert_eq!(Frame::<ServerMsg>::decode(frame.encode()).unwrap(), frame);
    }
}

#[test]
fn trailing_bytes_are_rejected() {
    let mut raw = Frame::new(1, ClientMsg::Ping).encode().to_vec();
    raw.push(0);
    assert_eq!(
        Frame::<ClientMsg>::decode(Bytes::from(raw)),
        Err(WireError::TrailingBytes)
    );
}

#[test]
fn directions_do_not_mix() {
    let raw = Frame::new(0, ServerMsg::Ready).encode();
    assert!(matches!(
        Frame::<ClientMsg>::decode(raw),
        Err(WireError::UnknownKind(_))
    ));
}

#[test]
fn malformed_link_and_bool_are_rejected() {
    let mut raw = Frame::new(1, ClientMsg::ResolveLink { link: link() })
        .encode()
        .to_vec();
    raw[5] = b'A';
    assert_eq!(
        Frame::<ClientMsg>::decode(Bytes::from(raw)),
        Err(WireError::Malformed)
    );

    let mut raw = Frame::new(
        1,
        ClientMsg::Send {
            to: PeerId([0; 32]),
            device: DeviceId::FIRST,
            want_ack: true,
            body: Bytes::new(),
        },
    )
    .encode()
    .to_vec();
    raw[SEND_HEADER_LEN - 1] = 7;
    assert_eq!(
        Frame::<ClientMsg>::decode(Bytes::from(raw)),
        Err(WireError::Malformed)
    );
}

/// Device zero names nobody: a frame carrying it is refused wherever a
/// device appears.
#[test]
fn device_zero_is_refused() {
    let hello = Frame::new(
        1,
        ClientMsg::Hello {
            version: 3,
            peer: PeerId([1; 32]),
            device: DeviceId(0),
        },
    );
    assert_eq!(
        Frame::<ClientMsg>::decode(hello.encode()),
        Err(WireError::Malformed)
    );
    let send = Frame::new(
        1,
        ClientMsg::Send {
            to: PeerId([1; 32]),
            device: DeviceId(0),
            want_ack: false,
            body: Bytes::new(),
        },
    )
    .encode();
    assert_eq!(
        Frame::<ClientMsg>::decode(send.clone()),
        Err(WireError::Malformed)
    );
    assert!(peek_send(&send).is_none());
    let recv = encode_recv(&PeerId([1; 32]), DeviceId(0), b"");
    assert_eq!(Frame::<ServerMsg>::decode(recv), Err(WireError::Malformed));
}

#[test]
fn oversized_inbox_item_is_rejected() {
    let frame = Frame::new(
        1,
        ClientMsg::InboxPut {
            inbox: [0; 32],
            item: Bytes::from(vec![0; MAX_INBOX_ITEM_LEN + 1]),
        },
    );
    assert_eq!(
        Frame::<ClientMsg>::decode(frame.encode()),
        Err(WireError::TooLarge)
    );
}

#[test]
fn recv_hot_path_matches_generic_encoder() {
    let from = PeerId([9; 32]);
    let body = Bytes::from_static(b"hello");
    let generic = Frame::new(
        0,
        ServerMsg::Recv {
            from,
            device: DeviceId(5),
            body: body.clone(),
        },
    )
    .encode();
    assert_eq!(encode_recv(&from, DeviceId(5), &body), generic);
}

#[test]
fn peek_send_matches_decoder_and_shares_buffer() {
    let frame = Frame::new(
        42,
        ClientMsg::Send {
            to: PeerId([1; 32]),
            device: DeviceId(9),
            want_ack: true,
            body: Bytes::from_static(b"xyz"),
        },
    )
    .encode();
    let view = peek_send(&frame).unwrap();
    assert_eq!(
        view,
        SendView {
            req_id: 42,
            to: PeerId([1; 32]),
            device: DeviceId(9),
            want_ack: true,
            body: Bytes::from_static(b"xyz"),
        }
    );
    assert_eq!(view.body.as_ptr(), frame[SEND_HEADER_LEN..].as_ptr());
    assert!(peek_send(&Frame::new(1, ClientMsg::Ping).encode()).is_none());
}

proptest! {
    #![proptest_config(proptest_config(256))]

    #[test]
    fn arbitrary_bytes_never_panic(data in prop::collection::vec(any::<u8>(), 0..512)) {
        let b = Bytes::from(data);
        let _ = Frame::<ClientMsg>::decode(b.clone());
        let _ = Frame::<ServerMsg>::decode(b.clone());
        let _ = peek_send(&b);
    }

    #[test]
    fn truncations_of_valid_frames_fail_cleanly(cut in 0usize..64) {
        for msg in client_samples() {
            let raw = Frame::new(7, msg).encode();
            if cut < raw.len() {
                let res = Frame::<ClientMsg>::decode(raw.slice(..cut));
                let is_send = raw[0] == 0x10 || raw[0] == 0x40;
                prop_assert!(res.is_err() || is_send);
            }
        }
    }
}

#[test]
fn push_endpoints_are_bounded() {
    let frame = |endpoint: String| {
        Frame::new(
            1,
            ClientMsg::PushRegister {
                secret: [1; 32],
                endpoint,
                p256dh: [4; 65],
                auth: [2; 16],
            },
        )
        .encode()
    };
    let longest = "a".repeat(cypher_wire::MAX_PUSH_ENDPOINT_LEN);
    Frame::<ClientMsg>::decode(frame(longest.clone())).unwrap();
    Frame::<ClientMsg>::decode(frame(format!("{longest}a"))).unwrap_err();
}
