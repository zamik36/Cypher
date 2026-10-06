#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unreachable,
    clippy::cast_possible_truncation,
    reason = "test code: panics are assertions and every fixture is small"
)]

mod harness;

use bytes::Bytes;
use cypher_core::{Command, Content, Event, FailReason, MediaKind, MessageStatus, Table, Vault};
use cypher_crypto::IdentitySeed;
use cypher_types::{FileId, PeerId};
use cypher_wire::{Frame, ServerMsg};
use harness::World;

const A: usize = 0;
const B: usize = 1;
const C: usize = 2;
const D: usize = 3;
const E: usize = 4;
const F: usize = 5;

fn paired() -> World {
    let mut w = World::new(2);
    w.pair(A, B);
    w
}

#[test]
fn join_link_establishes_session_both_ways_and_either_side_writes_first() {
    let mut w = paired();
    assert!(w.has_event(B, |e| matches!(
        e,
        Event::PeerAdded {
            initiated_by_us: true,
            ..
        }
    )));
    assert!(w.has_event(A, |e| matches!(
        e,
        Event::PeerAdded {
            initiated_by_us: false,
            ..
        }
    )));

    let from_host = w.send_text(A, B, "hi from host");
    let from_joiner = w.send_text(B, A, "hi from joiner");
    assert_eq!(w.texts(B), ["hi from host"]);
    assert_eq!(w.texts(A), ["hi from joiner"]);
    assert_eq!(w.status_of(A, from_host), Some(MessageStatus::Delivered));
    assert_eq!(w.status_of(B, from_joiner), Some(MessageStatus::Delivered));
}

/// A contact's name lives with its session on this device: it survives a
/// restart and a new session with the same person, and goes with the chat.
#[test]
fn contacts_learn_the_name_each_side_goes_by() {
    let mut w = World::new(3);
    w.command(
        A,
        Command::SetProfileName {
            name: Some(
                " Alice
"
                .into(),
            ),
        },
    );
    w.pair(A, B);
    let (a, b) = (w.peer(A), w.peer(B));
    assert_eq!(w.contact_name(B, a).as_deref(), Some("Alice"));
    assert!(w.has_event(B, |e| matches!(
        e,
        Event::PeerProfile { peer, name: Some(n) } if *peer == a && n == "Alice"
    )));
    assert_eq!(w.contact_name(A, b), None, "B goes by no name yet");

    // A new name reaches contacts already greeted.
    w.command(
        B,
        Command::SetProfileName {
            name: Some("Bob".into()),
        },
    );
    assert_eq!(w.contact_name(A, b).as_deref(), Some("Bob"));
    w.command(
        A,
        Command::SetProfileName {
            name: Some("Alicia".into()),
        },
    );
    assert_eq!(w.contact_name(B, a).as_deref(), Some("Alicia"));

    // Both the name and what contacts said survive restarts.
    w.restart(A);
    w.restart(B);
    assert_eq!(w.contact_name(B, a).as_deref(), Some("Alicia"));
    w.pair(A, C);
    assert_eq!(w.contact_name(C, a).as_deref(), Some("Alicia"));

    w.command(A, Command::SetProfileName { name: None });
    assert_eq!(w.contact_name(B, a), None);
    assert_eq!(w.contact_name(C, a), None);
}

#[test]
fn a_second_joiner_on_a_used_invite_waits_as_a_request() {
    let mut w = World::new(3);
    w.command(
        A,
        Command::SetProfileName {
            name: Some("Alice".into()),
        },
    );
    let link = w.create_link(A);
    w.command(B, Command::JoinLink { link: link.clone() });
    let (a, b, c) = (w.peer(A), w.peer(B), w.peer(C));
    assert!(w.has_event(
        A,
        |e| matches!(e, Event::PeerAdded { peer, .. } if *peer == b)
    ));
    assert!(!w.contact(A, b).request);
    assert_eq!(w.contact_name(B, a).as_deref(), Some("Alice"));

    // The same invite again, say intercepted: a request, told nothing.
    w.command(C, Command::JoinLink { link });
    assert!(w.has_event(
        A,
        |e| matches!(e, Event::ContactRequest { peer } if *peer == c)
    ));
    assert!(!w.has_event(
        A,
        |e| matches!(e, Event::PeerAdded { peer, .. } if *peer == c)
    ));
    assert!(w.contact(A, c).request);
    assert_eq!(w.contact_name(C, a), None, "no Hello before accepting");
    w.send_text(C, A, "hi, it's me");
    assert!(w.texts(A).contains(&"hi, it's me".to_owned()));

    w.command(A, Command::AcceptContact { peer: c });
    assert!(!w.contact(A, c).request);
    assert_eq!(w.contact_name(C, a).as_deref(), Some("Alice"));
    w.restart(A);
    assert!(!w.contact(A, c).request);
}

/// The server's one-time prekeys are not the host's (its saved state fell
/// behind what it published): one joiner fails, the host publishes a fresh
/// set and the next gets through; strangers cannot make it churn keys.
#[test]
fn prekeys_the_server_has_but_we_lack_are_replaced() {
    let mut w = World::new(6);
    let joined = |w: &World, i: usize| {
        let peer = w.peer(i);
        w.has_event(
            A,
            |e| matches!(e, Event::PeerAdded { peer: p, .. } if *p == peer),
        )
    };
    w.foreign_opks(A);
    w.pair(A, B);
    assert!(w.has_event(A, |e| matches!(
        e,
        Event::Warning {
            reason: FailReason::DecryptFailed
        }
    )));
    assert!(!joined(&w, B));
    w.pair(A, C);
    assert!(joined(&w, C));

    // Within the hour the keys stay as they are, however many try.
    w.foreign_opks(A);
    w.pair(A, D);
    w.pair(A, E);
    assert!(!joined(&w, D) && !joined(&w, E));
    w.advance(60 * 60_000);
    w.foreign_opks(A);
    w.pair(A, D);
    w.pair(A, F);
    assert!(joined(&w, F));
}

/// One side's session went back in time (its saved state was overwritten
/// by an older copy): the other cannot decrypt it, starts a fresh session
/// from published keys, and the two talk again, as the same contacts.
#[test]
fn a_session_that_drifted_apart_is_started_afresh() {
    let mut w = paired();
    w.send_text(A, B, "one");
    w.send_text(B, A, "two");
    let older = w.clients[B].kv.clone();
    w.send_text(A, B, "three");
    w.send_text(B, A, "four");
    w.send_text(A, B, "five");
    w.clients[B].kv = older;
    w.restart(B);

    w.send_text(B, A, "lost in the old session");
    assert!(w.has_event(A, |e| matches!(
        e,
        Event::Warning {
            reason: FailReason::DecryptFailed
        }
    )));
    let b = w.peer(B);
    w.send_text(B, A, "after the repair");
    w.send_text(A, B, "heard you");
    assert_eq!(w.texts(A).last().unwrap(), "after the repair");
    assert_eq!(w.texts(B).last().unwrap(), "heard you");
    assert!(!w.contact(A, b).request, "still a contact, not a stranger");
}

#[test]
fn a_blocked_contact_is_not_heard_until_unblocked() {
    let mut w = paired();
    let b = w.peer(B);
    w.command(A, Command::BlockPeer { peer: b });
    assert!(w.contact(A, b).blocked);
    let unheard = w.send_text(B, A, "are you there?");
    assert!(!w.texts(A).contains(&"are you there?".to_owned()));
    assert_ne!(w.status_of(B, unheard), Some(MessageStatus::Delivered));

    w.restart(A);
    assert!(w.contact(A, b).blocked, "blocking survives a restart");
    w.command(A, Command::UnblockPeer { peer: b });
    w.send_text(B, A, "now?");
    assert!(w.texts(A).contains(&"now?".to_owned()));
}

#[test]
fn contacts_keep_the_name_given_on_this_device() {
    let mut w = paired();
    let b = w.peer(B);
    w.command(
        A,
        Command::RenamePeer {
            peer: b,
            alias: Some("  Bob\u{7}  ".into()),
        },
    );
    assert_eq!(w.alias(A, b).as_deref(), Some("Bob"));

    w.restart(A);
    assert_eq!(w.alias(A, b).as_deref(), Some("Bob"));
    w.pair(A, B);
    assert_eq!(w.alias(A, b).as_deref(), Some("Bob"));
    w.send_text(A, B, "still talking");
    assert!(w.texts(B).contains(&"still talking".to_owned()));

    w.command(
        A,
        Command::RenamePeer {
            peer: b,
            alias: Some("   ".into()),
        },
    );
    assert_eq!(w.alias(A, b), None);
    w.command(A, Command::RemovePeer { peer: b });
    assert!(!w.has_session(A, b));
}

#[test]
fn host_can_message_first_right_after_join() {
    let mut w = World::new(2);
    w.pair(A, B);
    let id = w.send_text(A, B, "first!");
    assert_eq!(w.texts(B), ["first!"]);
    assert_eq!(w.status_of(A, id), Some(MessageStatus::Delivered));
}

#[test]
fn link_errors_are_reported_not_hung() {
    let mut w = World::new(2);
    w.command(
        B,
        Command::JoinLink {
            link: "not a link".into(),
        },
    );
    assert!(w.has_event(B, |e| matches!(
        e,
        Event::JoinFailed {
            reason: FailReason::InvalidLink,
            ..
        }
    )));

    w.command(
        B,
        Command::JoinLink {
            link: "not-a-link".into(),
        },
    );
    assert!(w.has_event(B, |e| matches!(
        e,
        Event::JoinFailed {
            reason: FailReason::InvalidLink,
            ..
        }
    )));

    w.command(
        B,
        Command::JoinLink {
            link: format!("{}-{}", "a".repeat(26), "b".repeat(26)),
        },
    );
    assert!(w.has_event(B, |e| matches!(
        e,
        Event::JoinFailed {
            reason: FailReason::NotFound,
            ..
        }
    )));

    let own = w.create_link(A);
    w.command(A, Command::JoinLink { link: own });
    assert!(w.has_event(A, |e| matches!(
        e,
        Event::JoinFailed {
            reason: FailReason::SelfLink,
            ..
        }
    )));
}

#[test]
fn offline_messages_go_through_sealed_inbox() {
    let mut w = paired();
    w.disconnect(B);
    let id = w.send_text(A, B, "while you were away");
    assert_eq!(w.status_of(A, id), Some(MessageStatus::Queued));
    assert!(w.texts(B).is_empty());

    w.connect(B);
    w.run();
    assert_eq!(w.texts(B), ["while you were away"]);
    assert_eq!(w.status_of(A, id), Some(MessageStatus::Delivered));
}

#[test]
fn messages_survive_restarts_without_key_reuse() {
    let mut w = paired();
    for i in 0..3 {
        w.send_text(A, B, &format!("before {i}"));
        w.send_text(B, A, &format!("reply {i}"));
    }
    w.restart(A);
    w.restart(B);
    w.send_text(A, B, "after restart");
    w.send_text(B, A, "still here");
    assert_eq!(w.texts(B).last().unwrap(), "after restart");
    assert_eq!(w.texts(A).last().unwrap(), "still here");

    let mut headers = std::collections::HashSet::new();
    for (_, raw) in &w.server.delivered {
        if let Ok(Frame {
            msg: ServerMsg::Recv { from, body },
            ..
        }) = Frame::<ServerMsg>::decode(raw.clone())
            && body.first() == Some(&0)
        {
            let init_len = if body[1] == 1 {
                32 + 32 + 4 + 5 + 64 - if body[2 + 68] == 0 { 4 } else { 0 }
            } else {
                0
            };
            let header = body.slice(2 + init_len..2 + init_len + 40);
            assert!(headers.insert((from, header)), "ratchet header reused");
        }
    }
}

#[test]
fn queued_outbox_is_delivered_after_sender_restart() {
    let mut w = paired();
    w.disconnect(A);
    let id = w.send_text(A, B, "written offline");
    assert_eq!(w.status_of(A, id), None);
    w.restart(A);
    assert_eq!(w.texts(B), ["written offline"]);
    assert_eq!(w.status_of(A, id), Some(MessageStatus::Delivered));
}

#[test]
fn a_message_deleted_before_it_went_out_is_never_sent() {
    let mut w = paired();
    w.disconnect(A);
    let kept = w.send_text(A, B, "this one goes");
    let gone = w.send_text(A, B, "never mind");
    w.command(A, Command::DiscardOutgoing { msg_id: gone });
    w.restart(A);
    assert_eq!(w.texts(B), ["this one goes"]);
    assert_eq!(w.status_of(A, kept), Some(MessageStatus::Delivered));
    assert_eq!(w.status_of(A, gone), None);
}

fn offer_file(
    w: &mut World,
    from: usize,
    to: usize,
    data: Vec<u8>,
    name: &str,
    kind: MediaKind,
) -> FileId {
    let file_id = w.file_id();
    let msg_id = w.msg_id();
    let peer = w.peer(to);
    let size = data.len() as u64;
    w.clients[from].sources.insert(file_id, data);
    w.command(
        from,
        Command::SendFile {
            peer,
            msg_id,
            file_id,
            name: name.into(),
            mime: "application/octet-stream".into(),
            size,
            kind,
            inline: None,
        },
    );
    file_id
}

/// Decrypts a client's sealed media copy the way a player would.
fn play(w: &World, i: usize, file_id: FileId) -> Option<Vec<u8>> {
    let c = &w.clients[i];
    let vault = Vault::new(IdentitySeed(c.seed).derive_storage_key());
    let raw = c.kv.get(&(Table::Media as u8, file_id.to_vec()))?;
    let key = vault.open_media(&file_id, raw).ok()?;
    key.open_all(c.sinks.get(&file_id)?).ok()
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 31 % 251) as u8).collect()
}

#[test]
fn file_transfer_with_accept_and_sanitized_name() {
    let mut w = paired();
    let data = pattern(1024 * 1024 + 3);
    let file_id = offer_file(
        &mut w,
        A,
        B,
        data.clone(),
        "../../evil/../payload.bin",
        MediaKind::File,
    );

    let offered_name = w.clients[B].events.iter().find_map(|e| match e {
        Event::TransferOffered { name, .. } => Some(name.clone()),
        _ => None,
    });
    assert_eq!(offered_name.as_deref(), Some("payload.bin"));
    assert!(!w.clients[B].sinks.contains_key(&file_id));

    w.command(B, Command::AcceptFile { file_id });
    assert_eq!(w.clients[B].sinks[&file_id], data);
    assert_eq!(w.clients[B].closed.get(&file_id), Some(&true));
    assert!(w.has_event(A, |e| matches!(e, Event::TransferComplete { .. })));
    assert!(w.has_event(B, |e| matches!(e, Event::TransferComplete { .. })));
}

#[test]
fn lossy_transfer_completes_through_retransmission() {
    let mut w = paired();
    w.server.drop_chunks_every = Some(3);
    let data = pattern(3 * 1024 * 1024);
    let file_id = offer_file(&mut w, A, B, data.clone(), "big.bin", MediaKind::File);
    w.command(B, Command::AcceptFile { file_id });
    w.advance(60_000);
    assert_eq!(w.clients[B].sinks[&file_id], data);
    assert!(w.has_event(A, |e| matches!(e, Event::TransferComplete { .. })));
}

#[test]
fn transfer_resumes_after_receiver_restart() {
    let mut w = paired();
    let data = pattern(2 * 1024 * 1024);
    w.server.drop_chunks_every = Some(2);
    let file_id = offer_file(&mut w, A, B, data.clone(), "resume.bin", MediaKind::File);
    w.command(B, Command::AcceptFile { file_id });
    w.server.drop_chunks_every = None;
    w.restart(B);
    w.advance(30_000);
    assert_eq!(w.clients[B].sinks[&file_id], data);
}

#[test]
fn unanswered_offer_is_announced_again_after_restart() {
    let mut w = paired();
    let data = pattern(300 * 1024);
    let file_id = offer_file(&mut w, A, B, data.clone(), "later.bin", MediaKind::File);
    w.clients[B].events.clear();
    w.restart(B);
    assert!(w.has_event(B, |e| matches!(
        e,
        Event::TransferOffered { file_id: id, name, size, .. }
            if *id == file_id && name == "later.bin" && *size == data.len() as u64
    )));

    w.command(B, Command::AcceptFile { file_id });
    w.advance(10_000);
    assert_eq!(w.clients[B].sinks[&file_id], data);

    // Accepted or done: nothing to announce any more.
    w.clients[B].events.clear();
    w.restart(B);
    assert!(!w.has_event(B, |e| matches!(e, Event::TransferOffered { .. })));
}

#[test]
fn voice_message_is_auto_accepted_and_stored_sealed_identically() {
    let mut w = paired();
    let voice = pattern(200 * 1024);
    let file_id = offer_file(
        &mut w,
        A,
        B,
        voice.clone(),
        "voice.webm",
        MediaKind::Voice {
            duration_ms: 12_000,
            waveform: vec![7; 64],
        },
    );
    let a_copy = &w.clients[A].sinks[&file_id];
    let b_copy = &w.clients[B].sinks[&file_id];
    assert_eq!(
        a_copy, b_copy,
        "sender and receiver keep identical sealed media"
    );
    assert_ne!(
        &b_copy[..voice.len()],
        &voice[..],
        "media is not stored in clear"
    );
    assert_eq!(w.clients[B].closed.get(&file_id), Some(&true));
    assert_eq!(play(&w, A, file_id), Some(voice.clone()));
    assert_eq!(play(&w, B, file_id), Some(voice));
}

#[test]
fn inline_video_note_travels_inside_the_message() {
    let mut w = paired();
    let note = pattern(20 * 1024);
    let file_id = w.file_id();
    let msg_id = w.msg_id();
    let peer = w.peer(B);
    w.command(
        A,
        Command::SendFile {
            peer,
            msg_id,
            file_id,
            name: "note.mp4".into(),
            mime: "video/mp4".into(),
            size: note.len() as u64,
            kind: MediaKind::VideoNote {
                duration_ms: 3000,
                poster: vec![1; 128],
            },
            inline: Some(note.clone()),
        },
    );
    assert_eq!(w.clients[A].sinks[&file_id], w.clients[B].sinks[&file_id]);
    assert_eq!(play(&w, B, file_id), Some(note));
    assert!(w.clients[B].events.iter().any(|e| matches!(
        e,
        Event::Message(m) if matches!(&m.content, Content::File { kind: MediaKind::VideoNote { duration_ms: 3000, .. }, .. })
    )));
    assert_eq!(w.status_of(A, msg_id), Some(MessageStatus::Delivered));
}

#[test]
fn server_forgeries_are_ignored() {
    let mut w = paired();
    let data = pattern(300 * 1024);
    let file_id = offer_file(&mut w, A, B, data.clone(), "x.bin", MediaKind::File);
    let a = w.peer(A);
    let b = w.peer(B);

    let mut forged_chunk = vec![1u8];
    forged_chunk.extend_from_slice(file_id.as_bytes());
    forged_chunk.extend_from_slice(&0u32.to_le_bytes());
    forged_chunk.extend_from_slice(&[0xAB; 64]);
    w.command(B, Command::AcceptFile { file_id });
    w.inject(
        B,
        Frame::new(
            0,
            ServerMsg::Recv {
                from: a,
                body: Bytes::from(forged_chunk),
            },
        )
        .encode(),
    );

    let mut forged_ack = vec![2u8];
    forged_ack.extend_from_slice(file_id.as_bytes());
    forged_ack.extend_from_slice(&u32::MAX.to_le_bytes());
    forged_ack.extend_from_slice(&0u64.to_le_bytes());
    forged_ack.extend_from_slice(&[0; 16]);
    w.inject(
        A,
        Frame::new(
            0,
            ServerMsg::Recv {
                from: b,
                body: Bytes::from(forged_ack),
            },
        )
        .encode(),
    );

    w.inject(
        B,
        Frame::new(
            0,
            ServerMsg::Recv {
                from: PeerId([5; 32]),
                body: Bytes::from_static(b"\x00\x00junk"),
            },
        )
        .encode(),
    );
    w.advance(5_000);
    assert_eq!(w.clients[B].sinks[&file_id], data);
}

#[test]
fn replayed_init_message_does_not_reset_the_session() {
    let mut w = paired();
    let first_to_a = w
        .server
        .delivered
        .iter()
        .find(|(to, raw)| *to == A && raw.len() > 45 && raw[5 + 32] == 0 && raw[5 + 33] == 1)
        .map(|(_, raw)| raw.clone())
        .expect("initial message with init header");
    w.send_text(A, B, "one");
    w.inject(A, first_to_a);
    w.send_text(B, A, "two");
    w.send_text(A, B, "three");
    assert_eq!(w.texts(A), ["two"]);
    assert_eq!(w.texts(B), ["one", "three"]);
}

#[test]
fn simultaneous_mutual_join_converges() {
    let mut w = World::new(2);
    let link_a = w.create_link(A);
    let link_b = w.create_link(B);
    w.clients[A].events.clear();
    w.clients[B].events.clear();
    w.command(B, Command::JoinLink { link: link_a });
    w.command(A, Command::JoinLink { link: link_b });
    w.send_text(A, B, "ping");
    w.send_text(B, A, "pong");
    assert_eq!(w.texts(B), ["ping"]);
    assert_eq!(w.texts(A), ["pong"]);
}

#[test]
fn cancelled_media_forgets_its_playback_key() {
    let mut w = paired();
    w.server.drop_chunks_every = Some(1);
    let file_id = offer_file(
        &mut w,
        A,
        B,
        pattern(512 * 1024),
        "voice.webm",
        MediaKind::Voice {
            duration_ms: 30_000,
            waveform: vec![1; 64],
        },
    );
    let media_key = (Table::Media as u8, file_id.to_vec());
    assert!(w.clients[A].kv.contains_key(&media_key));
    assert!(w.clients[B].kv.contains_key(&media_key));
    w.command(A, Command::CancelTransfer { file_id });
    assert!(!w.clients[A].kv.contains_key(&media_key));
    assert!(!w.clients[B].kv.contains_key(&media_key));
    assert_eq!(w.clients[B].closed.get(&file_id), Some(&false));
}

#[test]
fn cancel_stops_transfer_on_both_sides() {
    let mut w = paired();
    let file_id = offer_file(
        &mut w,
        A,
        B,
        pattern(4 * 1024 * 1024),
        "c.bin",
        MediaKind::File,
    );
    w.command(B, Command::CancelTransfer { file_id });
    assert!(w.has_event(A, |e| matches!(
        e,
        Event::TransferFailed {
            reason: FailReason::Cancelled,
            ..
        }
    )));
    assert!(w.has_event(B, |e| matches!(
        e,
        Event::TransferFailed {
            reason: FailReason::Cancelled,
            ..
        }
    )));
}

#[test]
fn missing_source_fails_transfer_cleanly() {
    let mut w = paired();
    let file_id = offer_file(&mut w, A, B, pattern(10), "gone.bin", MediaKind::File);
    w.clients[A].sources.clear();
    w.command(B, Command::AcceptFile { file_id });
    assert!(w.has_event(A, |e| matches!(
        e,
        Event::TransferFailed {
            reason: FailReason::SourceUnavailable,
            ..
        }
    )));
    assert!(w.has_event(B, |e| matches!(
        e,
        Event::TransferFailed {
            reason: FailReason::Cancelled,
            ..
        }
    )));
}

#[test]
fn inbox_traffic_goes_only_through_the_onion_relay() {
    let mut w = paired();
    w.disconnect(B);
    let id = w.send_text(A, B, "sealed twice");
    w.connect(B);
    w.run();
    assert_eq!(w.texts(B), ["sealed twice"]);
    assert_eq!(w.status_of(A, id), Some(MessageStatus::Delivered));
    assert!(w.server.onion_inbox_ops >= 3);
    assert_eq!(w.server.session_inbox_ops, 0);
}

#[test]
fn inbox_falls_back_to_session_only_when_allowed() {
    let mut w = World::new(2);
    w.relay_up = false;
    w.disconnect(A);
    w.connect(A);
    w.run();
    assert_eq!(w.server.session_inbox_ops, 0, "waits for the relay first");
    w.advance(11_000);
    let before = w.server.session_inbox_ops;
    assert!(before > 0, "fallback used the session");

    w.command(
        A,
        Command::SetAnonymity {
            require_onion: true,
        },
    );
    w.command(A, Command::FetchInbox);
    w.advance(1_000);
    assert_eq!(
        w.server.session_inbox_ops, before,
        "no leak once onion is required"
    );
}

/// A server that answers a share link with another identity (its own, or
/// someone it wants in the middle) is caught by the link's fingerprint.
#[test]
fn a_link_answered_with_another_identity_is_refused() {
    let mut w = World::new(3);
    let link = w.create_link(A);
    w.hijack_link(&link, C);
    w.command(B, Command::JoinLink { link });
    assert!(w.has_event(B, |e| matches!(
        e,
        Event::JoinFailed {
            reason: FailReason::KeyMismatch,
            ..
        }
    )));
    let peers = w.clients[B].core.as_ref().unwrap().peers().count();
    assert_eq!(peers, 0, "no session with the impostor");
}

/// Reading a message tells the sender once and marks it read locally, so a
/// frontend that asks again only for unread messages stops asking.
#[test]
fn reading_marks_messages_read_on_both_sides() {
    let mut w = paired();
    let id = w.send_text(A, B, "read me");
    let sender = w.peer(A);
    w.command(
        B,
        Command::MarkRead {
            peer: sender,
            ids: vec![id],
        },
    );
    assert_eq!(
        w.status_of(B, id),
        Some(MessageStatus::Read),
        "the reader's copy"
    );
    assert_eq!(
        w.status_of(A, id),
        Some(MessageStatus::Read),
        "the sender's copy"
    );
}
