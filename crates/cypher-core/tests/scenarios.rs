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
use cypher_types::{DeviceId, FileId, PeerId};
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

/// The host lost the prekeys a joiner used (the joiner fetched them just
/// before the host replaced its set): the host cannot answer, so after a
/// while the joiner starts again from the host's current keys, and what it
/// wrote in the meantime arrives.
#[test]
fn a_joiner_whose_prekey_is_gone_starts_again() {
    let mut w = World::new(2);
    w.foreign_opks(A);
    w.pair(A, B);
    let b = w.peer(B);
    let waiting = w.send_text(B, A, "are you there?");
    assert!(!w.texts(A).contains(&"are you there?".to_owned()));

    w.advance(5 * 60_000);
    assert!(w.has_event(
        A,
        |e| matches!(e, Event::PeerAdded { peer, .. } if *peer == b)
    ));
    assert_eq!(w.texts(A), ["are you there?"]);
    assert_eq!(w.status_of(B, waiting), Some(MessageStatus::Delivered));
}

/// A joiner the host has not accepted gets no answer to its Hello, and that
/// alone is no reason to spend the host's prekeys on starting again.
#[test]
fn a_joiner_waiting_to_be_accepted_does_not_start_again() {
    let mut w = World::new(3);
    let link = w.create_link(A);
    w.command(B, Command::JoinLink { link: link.clone() });
    w.command(C, Command::JoinLink { link });
    let fetches = w.server.key_fetches;
    w.advance(30 * 60_000);
    assert_eq!(w.server.key_fetches, fetches, "no new keys fetched");
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

    let lost = w.send_text(B, A, "lost in the old session");
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
    // What the old session could not carry came again on the new one.
    assert!(w.texts(A).contains(&"lost in the old session".to_owned()));
    assert_eq!(w.status_of(B, lost), Some(MessageStatus::Delivered));
}

/// Rolls `i` back to `older` and restarts it, then has it write to `to`:
/// that message cannot be read, and `to` starts a fresh session.
fn drift(
    w: &mut World,
    i: usize,
    older: std::collections::BTreeMap<(u8, Vec<u8>), Vec<u8>>,
    to: usize,
) {
    w.clients[i].kv = older;
    w.restart(i);
    w.send_text(i, to, "from the past");
}

/// The server took a message the contact never got: it waits for their
/// receipt and goes again once the two have a fresh session.
#[test]
fn a_message_the_contact_never_got_is_sent_again() {
    let mut w = paired();
    w.lose_messages_from(A, 1);
    let lost = w.send_text(A, B, "into the void");
    assert!(!w.texts(B).contains(&"into the void".to_owned()));
    assert_eq!(w.status_of(A, lost), Some(MessageStatus::Sent));

    let older = w.clients[B].kv.clone();
    w.send_text(B, A, "moving on");
    drift(&mut w, B, older, A);
    assert!(w.texts(B).contains(&"into the void".to_owned()));
    assert_eq!(w.status_of(A, lost), Some(MessageStatus::Delivered));
}

/// The contact got it but their receipt was lost: the copy sent again is
/// not shown twice, and it is confirmed this time.
#[test]
fn a_message_sent_again_is_confirmed_not_repeated() {
    let mut w = paired();
    w.lose_messages_from(B, 1);
    let id = w.send_text(A, B, "did you get this?");
    assert_eq!(w.status_of(A, id), Some(MessageStatus::Sent));

    let older = w.clients[A].kv.clone();
    w.send_text(A, B, "hello?");
    drift(&mut w, A, older, B);
    let seen = w
        .texts(B)
        .iter()
        .filter(|t| *t == "did you get this?")
        .count();
    assert_eq!(seen, 1);
    assert_eq!(w.stored_copies(B, id), 1);
    assert_eq!(w.status_of(A, id), Some(MessageStatus::Delivered));
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
            msg: ServerMsg::Recv { from, body, .. },
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
                device: DeviceId::FIRST,
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
                device: DeviceId::FIRST,
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
                device: DeviceId::FIRST,
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
        .find(|(to, raw)| {
            let recv = Frame::<ServerMsg>::decode(raw.clone()).map(|f| f.msg);
            // A ratchet message (tag 0) carrying an init header (flag 1).
            *to == A
                && matches!(recv, Ok(ServerMsg::Recv { body, .. }) if body.starts_with(&[0, 1]))
        })
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

fn push_events(w: &World, i: usize) -> (bool, bool) {
    let key = w.has_event(
        i,
        |e| matches!(e, Event::PushKey { key } if key.as_slice() == [4; 65]),
    );
    (key, w.has_event(i, |e| matches!(e, Event::PushRegistered)))
}

/// A device asks to be woken: the key and the registration go through the
/// relay only, and a message for its inbox then signals it.
#[test]
fn push_is_set_up_through_the_relay_and_signals_an_absent_device() {
    let mut w = paired();
    w.command(B, Command::EnablePush);
    assert_eq!(push_events(&w, B), (true, false));
    w.command(
        B,
        Command::RegisterPush {
            endpoint: "https://ntfy.sh/up-b".into(),
            p256dh: [4; 65],
            auth: [1; 16],
        },
    );
    assert_eq!(push_events(&w, B), (true, true));
    let inbox = w.clients[B].core.as_ref().unwrap().inbox_id();
    assert_eq!(
        w.server.pushes.get(&inbox).map(String::as_str),
        Some("https://ntfy.sh/up-b")
    );
    assert_eq!(w.server.session_push_ops, 0, "never over the session");

    w.disconnect(B);
    w.send_text(A, B, "while you were away");
    assert_eq!(w.server.signals, [inbox]);

    w.connect(B);
    w.run();
    w.command(B, Command::DisablePush);
    assert!(!w.server.pushes.contains_key(&inbox));
    assert_eq!(w.server.session_push_ops, 0);
}

/// Without the relay nothing about push is said, not even over the session
/// it would otherwise fall back to; it goes once the relay is up.
#[test]
fn push_waits_for_the_relay() {
    let mut w = World::new(2);
    w.relay_up = false;
    w.disconnect(A);
    w.connect(A);
    w.run();
    w.command(A, Command::EnablePush);
    w.advance(15_000);
    assert_eq!(push_events(&w, A), (false, false));
    assert_eq!(w.server.session_push_ops, 0);

    w.relay_up = true;
    w.disconnect(A);
    w.connect(A);
    w.run();
    assert!(push_events(&w, A).0);
    assert_eq!(w.server.session_push_ops, 0);
}

/// Contacts stored before an identity could have several devices held
/// their one session inside: after the update it moves to its own row, as
/// the session with the contact's first device, and keeps working.
#[test]
fn sessions_stored_with_their_contact_move_out_and_keep_working() {
    let mut w = paired();
    w.send_text(A, B, "before");
    w.send_text(B, A, "heard");
    let b = w.peer(B);
    legacy::store_as_v4(&mut w, A, b, "Bob");
    let session_row = legacy::session_row(b);
    assert!(!w.clients[A].kv.contains_key(&session_row));

    w.restart(A);
    assert!(
        w.clients[A].kv.contains_key(&session_row),
        "the session has its own row"
    );
    assert_eq!(w.alias(A, b).as_deref(), Some("Bob"));
    w.send_text(A, B, "after the update");
    w.send_text(B, A, "still here");
    assert_eq!(w.texts(B).last().unwrap(), "after the update");
    assert_eq!(w.texts(A).last().unwrap(), "still here");
    assert!(!w.has_event(A, |e| matches!(
        e,
        Event::Warning {
            reason: FailReason::DecryptFailed | FailReason::Corrupted
        }
    )));
}

/// Every device of a contact gets what is sent to them, each on its own
/// session, and the sender hears back from them.
#[test]
fn every_device_of_a_contact_gets_the_message() {
    let mut w = World::new(2);
    let a2 = w.add_device(A, 2);
    w.pair(A, B);
    let sent = w.send_text(B, A, "to both");
    assert_eq!(w.texts(A), ["to both"]);
    assert_eq!(w.texts(a2), ["to both"]);
    assert_eq!(w.status_of(B, sent), Some(MessageStatus::Delivered));
    let b = w.peer(B);
    assert!(w.has_session(A, b) && w.has_session(a2, b));
}

/// A device added later asks its sibling what it knows; the sibling, so
/// learning of it, announces it to the contacts, who write to it too.
#[test]
fn a_new_device_is_announced_to_contacts() {
    let mut w = paired();
    let a2 = w.add_device(A, 2);
    let b = w.peer(B);
    assert!(
        !w.contact(a2, b).request,
        "the sibling told it of its contacts"
    );

    w.send_text(B, A, "to both");
    assert_eq!(w.texts(A).last().unwrap(), "to both");
    assert_eq!(w.texts(a2), ["to both"]);
}

/// A device that is away gets what was sent meanwhile from its own inbox,
/// even the very first message of a session with it.
#[test]
fn a_device_offline_reads_it_from_its_own_inbox() {
    let mut w = paired();
    let a2 = w.add_device(A, 2);
    w.advance(60 * 60_000);
    w.disconnect(a2);
    let sent = w.send_text(B, A, "while you were away");
    assert_eq!(w.status_of(B, sent), Some(MessageStatus::Delivered));
    w.connect(a2);
    w.advance(10_000);
    assert_eq!(w.texts(a2), ["while you were away"]);
    assert_eq!(
        w.status_of(B, sent),
        Some(MessageStatus::Delivered),
        "a late answer from one device does not take the status back"
    );
}

/// A device taken off its identity's list finds out and stops; contacts
/// learn of the new list and forget their session with it.
#[test]
fn a_device_taken_off_the_list_stops_and_is_forgotten() {
    let mut w = paired();
    let a2 = w.add_device(A, 2);
    w.advance(60 * 60_000);
    w.send_text(B, A, "to both");
    let (a, b) = (w.peer(A), w.peer(B));
    let a2_session = devices::session_row(a, 2);
    assert!(w.clients[B].kv.contains_key(&a2_session));

    let identity = IdentitySeed(w.clients[A].seed).derive_identity();
    let shorter = w.server.devices[&a]
        .without(&identity, DeviceId(2))
        .unwrap();
    w.server.devices.insert(a, shorter);
    w.advance(60 * 60_000);
    assert!(w.has_event(a2, |e| matches!(e, Event::DeviceUnlinked)));
    assert!(!w.has_event(A, |e| matches!(e, Event::DeviceUnlinked)));
    assert!(
        !w.clients[B].kv.contains_key(&a2_session),
        "{b:?} forgot the session with the removed device"
    );
    w.send_text(B, A, "to the one left");
    assert_eq!(w.texts(A).last().unwrap(), "to the one left");
}

/// A voice note is taken by every device of the contact at once, each at
/// its own pace, and is sent in full to each.
#[test]
fn a_voice_note_reaches_every_device() {
    let mut w = World::new(2);
    let b2 = w.add_device(B, 2);
    w.pair(B, A);
    let voice = pattern(200 * 1024);
    let kind = MediaKind::Voice {
        duration_ms: 5_000,
        waveform: vec![3; 64],
    };
    let file_id = offer_file(&mut w, A, B, voice, "voice.webm", kind);
    let a_copy = &w.clients[A].sinks[&file_id];
    for device in [B, b2] {
        assert_eq!(
            &w.clients[device].sinks[&file_id], a_copy,
            "device {device}"
        );
        assert_eq!(w.clients[device].closed.get(&file_id), Some(&true));
    }
    let completions = w.clients[A]
        .events
        .iter()
        .filter(|e| matches!(e, Event::TransferComplete { .. }))
        .count();
    assert_eq!(completions, 1, "the sender sees one transfer");
}

/// A file offered to a contact with two devices goes to the one that
/// accepts it; the other, accepting once it was sent, is told it is gone.
#[test]
fn a_file_accepted_after_it_was_sent_is_gone() {
    let mut w = World::new(2);
    let b2 = w.add_device(B, 2);
    w.pair(B, A);
    let data = pattern(300 * 1024);
    let file_id = offer_file(&mut w, A, B, data.clone(), "doc.pdf", MediaKind::File);
    for device in [B, b2] {
        assert!(w.has_event(device, |e| matches!(
            e,
            Event::TransferOffered { file_id: f, .. } if *f == file_id
        )));
    }
    w.command(b2, Command::AcceptFile { file_id });
    assert_eq!(w.clients[b2].sinks[&file_id], data);
    assert!(w.has_event(A, |e| matches!(e, Event::TransferComplete { .. })));

    w.command(B, Command::AcceptFile { file_id });
    assert!(w.has_event(B, |e| matches!(
        e,
        Event::TransferFailed {
            reason: FailReason::Cancelled,
            ..
        }
    )));
    assert!(w.clients[B].sinks.get(&file_id).is_none_or(|d| *d != data));
}

/// What one device sends shows on the identity's other devices as sent.
#[test]
fn a_message_sent_on_one_device_shows_on_the_other() {
    let mut w = paired();
    let a2 = w.add_device(A, 2);
    let b = w.peer(B);
    let sent = w.send_text(A, B, "from the first");
    let mirrored = w.clients[a2].events.iter().find_map(|e| match e {
        Event::Message(m) if m.msg_id == sent => Some(m.clone()),
        _ => None,
    });
    let mirrored = mirrored.expect("shown on the other device");
    assert!(mirrored.outgoing && mirrored.peer == b);
    assert!(matches!(&mirrored.content, Content::Text { text, .. } if text == "from the first"));
    assert_eq!(w.status_of(a2, sent), Some(MessageStatus::Delivered));
    assert_eq!(w.texts(B), ["from the first"], "the contact gets it once");
}

/// Reading on one device marks the messages read on the others too.
#[test]
fn reading_on_one_device_marks_it_read_on_the_other() {
    let mut w = paired();
    let a2 = w.add_device(A, 2);
    let msg = w.send_text(B, A, "read me");
    let b = w.peer(B);
    w.command(
        A,
        Command::MarkRead {
            peer: b,
            ids: vec![msg],
        },
    );
    assert_eq!(w.status_of(a2, msg), Some(MessageStatus::Read));
    assert_eq!(w.status_of(B, msg), Some(MessageStatus::Read));
}

/// Renaming or blocking a contact on one device does so on all of them.
#[test]
fn contacts_change_on_every_device() {
    let mut w = paired();
    let a2 = w.add_device(A, 2);
    let b = w.peer(B);
    w.command(
        A,
        Command::RenamePeer {
            peer: b,
            alias: Some("Bobby".into()),
        },
    );
    assert_eq!(w.alias(a2, b).as_deref(), Some("Bobby"));
    w.command(A, Command::BlockPeer { peer: b });
    assert!(w.contact(a2, b).blocked);
    w.send_text(B, A, "anyone?");
    assert!(w.texts(a2).is_empty(), "blocked on the other device too");
}

/// An invite made on one device admits whoever joins by it on the others.
#[test]
fn an_invite_made_on_one_device_admits_on_another() {
    let mut w = World::new(2);
    let a2 = w.add_device(A, 2);
    w.pair(A, B);
    let b = w.peer(B);
    assert!(!w.contact(A, b).request);
    assert!(
        !w.contact(a2, b).request,
        "a contact there too, not a request"
    );
}

mod devices {
    use cypher_core::{Table, session_key};
    use cypher_types::{Addr, DeviceId, PeerId};

    pub(crate) fn session_row(peer: PeerId, device: u32) -> (u8, Vec<u8>) {
        let addr = Addr::new(peer, DeviceId(device));
        (Table::Sessions as u8, session_key(addr))
    }
}

/// Records as releases before devices stored them.
mod legacy {
    use cypher_core::{Record, Table, Vault, session_key};
    use cypher_crypto::IdentitySeed;
    use cypher_types::{Addr, DeviceId, PeerId};
    use serde::{Deserialize, Serialize};

    use crate::harness::World;

    #[derive(Serialize, Deserialize)]
    struct SessionV1 {
        ratchet: Vec<u8>,
        pending_init: Vec<u8>,
        accepted_ephemerals: Vec<[u8; 32]>,
        inbox: Option<[u8; 32]>,
        hello_sent: bool,
    }

    impl Record for SessionV1 {
        const VERSION: u8 = 1;
    }

    /// A contact as stored now.
    #[derive(Serialize, Deserialize)]
    pub(crate) struct Contact {
        identity_dh: [u8; 32],
        pub(crate) alias: Option<String>,
        name: Option<String>,
        request: bool,
        blocked: bool,
        via: Option<String>,
        devices: Option<Vec<u8>>,
        inboxes: Vec<(u32, [u8; 32])>,
    }

    impl Record for Contact {
        const VERSION: u8 = 6;
    }

    #[derive(Serialize, Deserialize)]
    struct PeerV4 {
        ratchet: Vec<u8>,
        pending_init: Vec<u8>,
        accepted_ephemerals: Vec<[u8; 32]>,
        identity_dh: [u8; 32],
        inbox: Option<[u8; 32]>,
        hello_sent: bool,
        alias: Option<String>,
        name: Option<String>,
        request: bool,
        blocked: bool,
        via: Option<String>,
    }

    impl Record for PeerV4 {
        const VERSION: u8 = 4;
    }

    pub(crate) fn session_row(peer: PeerId) -> (u8, Vec<u8>) {
        let addr = Addr::new(peer, DeviceId::FIRST);
        (Table::Sessions as u8, session_key(addr))
    }

    /// Rewrites client `i`'s contact `peer` and its session as one version 4
    /// record, with `alias`.
    pub(crate) fn store_as_v4(w: &mut World, i: usize, peer: PeerId, alias: &str) {
        let vault = Vault::new(IdentitySeed(w.clients[i].seed).derive_storage_key());
        let (session_row, peer_row) = (session_row(peer), (Table::Peers as u8, peer.to_vec()));
        let kv = &mut w.clients[i].kv;
        let sealed_session = kv.remove(&session_row).unwrap();
        let session: SessionV1 = vault
            .open(Table::Sessions, &session_row.1, &sealed_session)
            .unwrap();
        let contact: Contact = vault
            .open(Table::Peers, &peer_row.1, &kv[&peer_row])
            .unwrap();
        let old = PeerV4 {
            ratchet: session.ratchet,
            pending_init: session.pending_init,
            accepted_ephemerals: session.accepted_ephemerals,
            identity_dh: contact.identity_dh,
            inbox: session.inbox,
            hello_sent: session.hello_sent,
            alias: Some(alias.to_owned()),
            name: contact.name,
            request: contact.request,
            blocked: contact.blocked,
            via: contact.via,
        };
        let sealed = vault.seal(Table::Peers, &peer_row.1, &old, &mut rand::rngs::OsRng);
        kv.insert(peer_row, sealed);
    }
}
