//! File transfers: which way chunks may come, pausing while offline and
//! going on after, how often progress is told, cancelling while paused.

use std::collections::BTreeSet;

use bytes::Bytes;
use cypher_core::envelope::FILE_CHUNK_SIZE;
use cypher_core::{Command, Event, FailReason, MediaKind};
use cypher_crypto::IdentitySeed;
use cypher_types::{Addr, FileId};
use cypher_wire::{ClientMsg, Frame, ServerMsg};

use super::{addr, is_ack, is_chunk};
use crate::harness::{Fault, Rule, Seen, World};
use crate::{A, B, offer_file, paired, pattern};

/// Drops every chunk on its way, as a lossy path would.
fn lose_chunks(w: &mut World) {
    w.server.faults.push(Rule::always(
        |m| matches!(m, ClientMsg::Send { body, .. } if is_chunk(body)),
        Fault::Ignore,
    ));
}

/// `body` from `from` as client `to` gets it sealed: from an inbox, or
/// delivered without the sender's name.
fn sealed_to(w: &World, from: Addr, to: usize, body: &[u8]) -> Bytes {
    let identity = IdentitySeed(w.clients[to].seed).derive_identity();
    let mut plain = from.peer.as_bytes().to_vec();
    plain.extend_from_slice(&from.device.0.to_le_bytes());
    plain.extend_from_slice(body);
    let item = cypher_crypto::sealed::seal(
        &identity.dh_public_key().to_bytes(),
        &plain,
        &mut rand::rngs::OsRng,
    )
    .unwrap();
    Frame::new(
        0,
        ServerMsg::RecvSealed {
            item: Bytes::from(item),
        },
    )
    .encode()
}

/// `body` from `from` as it comes over the session, stamped by the gateway.
fn over_session(from: Addr, body: &Bytes) -> Bytes {
    Frame::new(
        0,
        ServerMsg::Recv {
            from: from.peer,
            device: from.device,
            body: body.clone(),
        },
    )
    .encode()
}

fn completed(w: &World, i: usize, file_id: FileId) -> bool {
    w.has_event(
        i,
        |e| matches!(e, Event::TransferComplete { file_id: id } if *id == file_id),
    )
}

/// The bodies of what `from` sent to client `to` over the session that
/// `pick` chooses.
fn bodies(w: &World, from: usize, to: usize, pick: fn(&[u8]) -> bool) -> Vec<Bytes> {
    w.sends_to(from, addr(w, to))
        .into_iter()
        .filter(|b| pick(b))
        .collect()
}

/// A chunk that comes sealed, as from an inbox, is ignored; the same chunk
/// over the session is taken.
#[test]
fn chunks_that_come_sealed_are_ignored() {
    let mut w = paired();
    lose_chunks(&mut w);
    let data = pattern(300 * 1024);
    let file_id = offer_file(&mut w, A, B, data.clone(), "f.bin", MediaKind::File);
    w.command(B, Command::AcceptFile { file_id });
    let chunks = bodies(&w, A, B, is_chunk);
    assert!(!chunks.is_empty());
    w.server.faults.clear();

    let from = addr(&w, A);
    for chunk in &chunks {
        let sealed = sealed_to(&w, from, B, chunk);
        w.inject(B, sealed);
    }
    assert!(w.clients[B].sinks[&file_id].iter().all(|b| *b == 0));
    assert!(!completed(&w, B, file_id));

    for chunk in &chunks {
        w.inject(B, over_session(from, chunk));
    }
    assert_eq!(w.clients[B].sinks[&file_id], data);
    assert!(completed(&w, B, file_id));
}

/// An acknowledgement that comes sealed is ignored; the same one over the
/// session is taken.
#[test]
fn acks_that_come_sealed_are_ignored() {
    let mut w = paired();
    w.server.faults.push(Rule::always(
        |m| matches!(m, ClientMsg::Send { body, .. } if is_ack(body)),
        Fault::Ignore,
    ));
    let data = pattern(300 * 1024);
    let file_id = offer_file(&mut w, A, B, data.clone(), "f.bin", MediaKind::File);
    w.command(B, Command::AcceptFile { file_id });
    assert_eq!(w.clients[B].sinks[&file_id], data);
    let acks = bodies(&w, B, A, is_ack);
    assert!(!acks.is_empty());
    w.server.faults.clear();

    let from = addr(&w, B);
    for ack in &acks {
        let sealed = sealed_to(&w, from, A, ack);
        w.inject(A, sealed);
    }
    assert!(!completed(&w, A, file_id));
    for ack in &acks {
        w.inject(A, over_session(from, ack));
    }
    assert!(completed(&w, A, file_id));
}

/// The chunk reads client `i` asked for in the log.
fn reads(w: &World, i: usize, file_id: FileId) -> usize {
    let log = w.log.as_ref().expect("the log is kept");
    log.iter()
        .filter(|batch| batch.client == i)
        .flat_map(|batch| &batch.effects)
        .filter(|e| matches!(e, Seen::ReadChunk(id) if *id == file_id))
        .count()
}

/// While offline, nothing of a file is read or sent, whatever the timers
/// say; once back it goes on at once, without waiting for one.
#[test]
fn a_transfer_pauses_while_offline_and_goes_on_once_back() {
    let mut w = paired();
    lose_chunks(&mut w);
    let data = pattern(2 * 1024 * 1024);
    let file_id = offer_file(&mut w, A, B, data.clone(), "f.bin", MediaKind::File);
    w.command(B, Command::AcceptFile { file_id });
    w.disconnect(A);
    w.log = Some(Vec::new());
    w.advance(60_000);
    assert_eq!(reads(&w, A, file_id), 0, "nothing read while offline");

    w.server.faults.clear();
    w.connect(A);
    w.run();
    assert!(reads(&w, A, file_id) > 0);
    assert_eq!(w.clients[B].sinks[&file_id], data);
    assert!(completed(&w, A, file_id) && completed(&w, B, file_id));
}

/// The index of a chunk body.
fn chunk_index(body: &[u8]) -> u32 {
    u32::from_le_bytes(body[17..21].try_into().unwrap())
}

/// The receiver coming back asks for the rest: each chunk it lacks is sent
/// once more, and none it has.
#[test]
fn a_receiver_back_online_gets_only_what_it_lacks() {
    let mut w = paired();
    w.server.drop_chunks_every = Some(2);
    let data = pattern(3 * 1024 * 1024);
    let file_id = offer_file(&mut w, A, B, data.clone(), "f.bin", MediaKind::File);
    w.command(B, Command::AcceptFile { file_id });
    let had: BTreeSet<u32> = w
        .server
        .delivered
        .iter()
        .filter(|(to, _)| *to == B)
        .filter_map(
            |(_, raw)| match Frame::<ServerMsg>::decode(raw.clone()).ok()?.msg {
                ServerMsg::Recv { body, .. } if is_chunk(&body) => Some(chunk_index(&body)),
                _ => None,
            },
        )
        .collect();
    let sent = bodies(&w, A, B, is_chunk).len();
    let total = u32::try_from(data.len().div_ceil(FILE_CHUNK_SIZE as usize)).unwrap();
    assert!(!had.is_empty() && had.len() < total as usize, "{had:?}");

    w.server.drop_chunks_every = None;
    w.disconnect(B);
    w.connect(B);
    w.run();
    let again: Vec<u32> = bodies(&w, A, B, is_chunk)[sent..]
        .iter()
        .map(|b| chunk_index(b))
        .collect();
    let lacked: Vec<u32> = (0..total).filter(|i| !had.contains(i)).collect();
    assert_eq!(again, lacked);
    assert_eq!(w.clients[B].sinks[&file_id], data);
}

/// When client `i` told the progress of `file_id`, and how far.
fn progress(w: &World, i: usize, file_id: FileId) -> Vec<(u64, u64)> {
    let log = w.log.as_ref().expect("the log is kept");
    log.iter()
        .filter(|batch| batch.client == i)
        .flat_map(|batch| {
            batch.effects.iter().filter_map(move |e| match e {
                Seen::Emit(Event::TransferProgress {
                    file_id: id, bytes, ..
                }) if *id == file_id => Some((batch.at, *bytes)),
                _ => None,
            })
        })
        .collect()
}

/// Progress is told at most every 100 ms per transfer and side, and only
/// ever grows.
#[test]
fn progress_is_told_at_most_every_100_ms() {
    let mut w = paired();
    w.log = Some(Vec::new());
    w.server.drop_chunks_every = Some(3);
    let data = pattern(4 * 1024 * 1024);
    let file_id = offer_file(&mut w, A, B, data.clone(), "f.bin", MediaKind::File);
    w.command(B, Command::AcceptFile { file_id });
    w.advance(60_000);
    assert_eq!(w.clients[B].sinks[&file_id], data);
    for i in [A, B] {
        let told = progress(&w, i, file_id);
        assert!(told.len() >= 2, "client {i}: {told:?}");
        for pair in told.windows(2) {
            let ((t0, b0), (t1, b1)) = (pair[0], pair[1]);
            assert!(t1 - t0 >= 100 && b1 >= b0, "client {i}: {told:?}");
        }
    }
}

/// A transfer cancelled while offline ends at once on this side; the other
/// side hears of it once back, and nothing more of it is sent.
#[test]
fn a_transfer_cancelled_while_offline_ends_on_both_sides() {
    let mut w = paired();
    lose_chunks(&mut w);
    let file_id = offer_file(
        &mut w,
        A,
        B,
        pattern(2 * 1024 * 1024),
        "f.bin",
        MediaKind::File,
    );
    w.command(B, Command::AcceptFile { file_id });
    w.disconnect(A);
    w.command(A, Command::CancelTransfer { file_id });
    let cancelled = |w: &World, i| {
        w.has_event(i, |e| {
            matches!(e, Event::TransferFailed { file_id: id, reason: FailReason::Cancelled }
                if *id == file_id)
        })
    };
    assert!(cancelled(&w, A), "at once");
    assert!(!cancelled(&w, B));

    w.server.faults.clear();
    let sent = bodies(&w, A, B, is_chunk).len();
    w.connect(A);
    w.advance(30_000);
    assert!(cancelled(&w, B));
    assert_eq!(w.clients[B].closed.get(&file_id), Some(&false));
    assert_eq!(bodies(&w, A, B, is_chunk).len(), sent, "nothing more sent");
}
