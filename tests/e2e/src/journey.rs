//! The user journey: pair through a link, chat, send a file, voice and video
//! notes, then deliver while the recipient is offline.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use cypher_client::{Client, ClientError, Config, Content};
use cypher_core::{Command, CommandId, Event, MediaKind, MessageStatus};
use cypher_crypto::IdentitySeed;
use cypher_types::{FileId, PeerId};
use p256::elliptic_curve::sec1::ToEncodedPoint as _;
use tempfile::TempDir;
use tokio::sync::mpsc::UnboundedReceiver;

const WAIT: Duration = Duration::from_secs(20);
/// How often [`eventually`] looks again.
const POLL: Duration = Duration::from_millis(20);

/// Waits until `check` holds, looking again every [`POLL`] for up to
/// [`WAIT`]: for what a client finishes on its IO thread after the event
/// that told of it.
async fn eventually(what: &str, mut check: impl AsyncFnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while !check().await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{what}: not within {WAIT:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Where clients connect: a gateway and the certificates they pin.
#[derive(Clone)]
pub struct Target {
    pub gateway_addr: String,
    pub tls: Arc<rustls::ClientConfig>,
}

impl Target {
    fn client_config(&self, dir: &Path) -> Config {
        Config {
            gateway_addr: self.gateway_addr.clone(),
            tls: Arc::clone(&self.tls),
            data_dir: dir.to_owned(),
            require_onion: true,
            tor: None,
        }
    }
}

/// Runs the whole journey with two fresh identities.
pub async fn run(target: &Target) {
    let mut a = Peer::start(
        target,
        IdentitySeed::generate(),
        tempfile::tempdir().unwrap(),
    )
    .await;
    let mut b = Peer::start(
        target,
        IdentitySeed::generate(),
        tempfile::tempdir().unwrap(),
    )
    .await;

    pair_through_a_link(&mut a, &mut b).await;
    chat_both_ways(&mut a, &mut b).await;
    let file_id = send_a_file(&mut a, &mut b).await;
    send_a_staged_copy(&mut a, &mut b).await;
    move_a_saved_file(&b, file_id).await;
    play_back_a_voice_note(&a, &mut b).await;
    stream_a_video_note(&mut a, &mut b).await;
    register_for_push(&mut b).await;
    deliver_offline_through_the_inbox(target, &mut a, b).await;
    a.client.shutdown().await;
}

/// A second device joins an identity by the offer it shows, through the real
/// gateway and signaling, with the history so far, and gets what a contact
/// sends from then on.
pub async fn linking(target: &Target) {
    let mut a = Peer::start(
        target,
        IdentitySeed::generate(),
        tempfile::tempdir().unwrap(),
    )
    .await;
    let mut b = Peer::start(
        target,
        IdentitySeed::generate(),
        tempfile::tempdir().unwrap(),
    )
    .await;
    pair_through_a_link(&mut a, &mut b).await;
    let a_id = a.seed.derive_identity().peer_id();
    let earlier = Command::SendText {
        peer: a_id,
        msg_id: cypher_types::MsgId([6; 16]),
        text: "before the laptop".into(),
        reply_to: None,
    };
    b.client.command(earlier).await.unwrap();
    assert_eq!(a.text_from_peer().await, "before the laptop");

    let waiting = cypher_client::provision(
        target.gateway_addr.clone(),
        Arc::clone(&target.tls),
        "Laptop",
    );
    link_from(&mut a, waiting.offer(), true).await;
    let linked = tokio::time::timeout(WAIT, waiting.linked())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(linked.seed.as_bytes(), a.seed.as_bytes());
    let mut laptop = Peer::start_device(
        target,
        linked.seed,
        linked.device,
        tempfile::tempdir().unwrap(),
    )
    .await;
    let b_id = b.seed.derive_identity().peer_id();
    assert!(
        has_text(&laptop, b_id, "before the laptop").await,
        "the laptop has the chat from before it was linked"
    );

    let hello = Command::SendText {
        peer: a_id,
        msg_id: cypher_types::MsgId([7; 16]),
        text: "to both of you".into(),
        reply_to: None,
    };
    b.client.command(hello).await.unwrap();
    assert_eq!(a.text_from_peer().await, "to both of you");
    assert_eq!(laptop.text_from_peer().await, "to both of you");
    for peer in [a, b, laptop] {
        peer.client.shutdown().await;
    }
}

/// The recovery phrase brings the profile to another install as a new
/// device: the first keeps working, the restored one gets the contacts from
/// it, and what a contact writes then reaches both.
pub async fn restoring(target: &Target) {
    let mut a = Peer::start(
        target,
        IdentitySeed::generate(),
        tempfile::tempdir().unwrap(),
    )
    .await;
    let mut b = Peer::start(
        target,
        IdentitySeed::generate(),
        tempfile::tempdir().unwrap(),
    )
    .await;
    pair_through_a_link(&mut a, &mut b).await;

    let kept = tempfile::tempdir().unwrap();
    let restored = cypher_client::IdentityStore::new(kept.path())
        .import(&a.seed.to_mnemonic(), "alice", "correct horse battery")
        .unwrap();
    assert_ne!(restored.device, cypher_types::DeviceId::FIRST);
    let mut phone = Peer::start_device(
        target,
        restored.seed,
        restored.device,
        tempfile::tempdir().unwrap(),
    )
    .await;

    // The contact hears of the new device once the first one, asked by it
    // for the contacts, learns of it and announces it.
    let a_id = a.seed.derive_identity().peer_id();
    let mut reached = false;
    for n in 0..40u8 {
        let hello = Command::SendText {
            peer: a_id,
            msg_id: cypher_types::MsgId([n; 16]),
            text: format!("to both of you #{n}"),
            reply_to: None,
        };
        b.client.command(hello).await.unwrap();
        if tokio::time::timeout(Duration::from_millis(500), phone.text_from_peer())
            .await
            .is_ok()
        {
            reached = true;
            break;
        }
    }
    assert!(reached, "the restored device never heard from the contact");
    assert!(a.text_from_peer().await.starts_with("to both of you"));
    for peer in [a, b, phone] {
        peer.client.shutdown().await;
    }
}

/// Whether `peer` keeps a message `text` from `from`, waiting a while: the
/// history may be taken in while the device was still signing in.
async fn has_text(peer: &Peer, from: PeerId, text: &str) -> bool {
    for _ in 0..100 {
        let history = peer.client.history(from, None, 10).await.unwrap();
        let found = history
            .iter()
            .any(|m| matches!(&m.content, Content::Text { text: t, .. } if t == text));
        if found {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    false
}

/// Links the device showing `offer` from `a`, trying again while the new
/// device's throwaway session is still signing in.
async fn link_from(a: &mut Peer, offer: &str, history: bool) {
    for _ in 0..50 {
        let link = Command::LinkDevice {
            id: CommandId(1),
            offer: offer.to_owned(),
            history,
        };
        a.client.command(link).await.unwrap();
        let linked = a
            .wait(|e| match e {
                Event::DeviceLinked { .. } => Some(true),
                Event::LinkFailed { .. } => Some(false),
                _ => None,
            })
            .await;
        if linked {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the new device was never reached");
}

/// A running client with its identity, event stream and data directory.
struct Peer {
    seed: IdentitySeed,
    dir: TempDir,
    client: Client,
    events: UnboundedReceiver<Event>,
}

impl Peer {
    async fn start(target: &Target, seed: IdentitySeed, dir: TempDir) -> Self {
        Self::start_device(target, seed, cypher_types::DeviceId::FIRST, dir).await
    }

    async fn start_device(
        target: &Target,
        seed: IdentitySeed,
        device: cypher_types::DeviceId,
        dir: TempDir,
    ) -> Self {
        let (client, events) = Client::start(&seed, device, target.client_config(dir.path()))
            .await
            .unwrap();
        let mut peer = Self {
            seed,
            dir,
            client,
            events,
        };
        peer.wait(|e| matches!(e, Event::Connected).then_some(()))
            .await;
        // The anonymous channel is up and the inbox read at least once, as
        // for anyone who has been online: only then does the server accept
        // offline messages for it.
        peer.wait(|e| matches!(e, Event::Onion { up: true }).then_some(()))
            .await;
        peer
    }

    /// Waits for the first event `pick` accepts; on timeout the panic lists
    /// what arrived instead.
    async fn wait<T>(&mut self, mut pick: impl FnMut(&Event) -> Option<T>) -> T {
        let mut skipped = Vec::new();
        let found = tokio::time::timeout(WAIT, async {
            loop {
                let event = self.events.recv().await.expect("client running");
                if let Some(v) = pick(&event) {
                    return v;
                }
                skipped.push(format!("{event:?}"));
            }
        })
        .await;
        found.unwrap_or_else(|_| panic!("no matching event within {WAIT:?}; got {skipped:#?}"))
    }

    async fn transfer_complete(&mut self, id: FileId) {
        self.wait(|e| {
            matches!(e, Event::TransferComplete { file_id } if *file_id == id).then_some(())
        })
        .await;
    }

    async fn text_from_peer(&mut self) -> String {
        self.wait(|e| match e {
            Event::Message(m) if !m.outgoing => match &m.content {
                Content::Text { text, .. } => Some(text.clone()),
                Content::File { .. } => None,
            },
            _ => None,
        })
        .await
    }
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251).to_le_bytes()[0]).collect()
}

async fn pair_through_a_link(a: &mut Peer, b: &mut Peer) {
    a.client
        .command(Command::CreateLink { id: CommandId(2) })
        .await
        .unwrap();
    let link = a
        .wait(|e| match e {
            Event::LinkCreated { link, .. } => Some(link.clone()),
            _ => None,
        })
        .await;
    b.client
        .command(Command::JoinLink {
            id: CommandId(3),
            link,
        })
        .await
        .unwrap();
    b.wait(|e| matches!(e, Event::PeerAdded { .. }).then_some(()))
        .await;
    a.wait(|e| matches!(e, Event::PeerAdded { .. }).then_some(()))
        .await;
}

async fn chat_both_ways(a: &mut Peer, b: &mut Peer) {
    let hi = a
        .client
        .send_text(b.client.peer_id(), "привет".into(), None)
        .await
        .unwrap();
    assert_eq!(b.text_from_peer().await, "привет");
    a.wait(|e| match e {
        Event::MessageStatus {
            msg_id,
            status: MessageStatus::Delivered,
        } if *msg_id == hi => Some(()),
        _ => None,
    })
    .await;
    // An answer carries what it answers.
    b.client
        .send_text(a.client.peer_id(), "hi back".into(), Some(hi))
        .await
        .unwrap();
    let answered = a
        .wait(|e| match e {
            Event::Message(m) if !m.outgoing => match &m.content {
                Content::Text { reply_to, .. } => Some(*reply_to),
                Content::File { .. } => None,
            },
            _ => None,
        })
        .await;
    assert_eq!(answered, Some(hi));

    let history = b
        .client
        .history(a.client.peer_id(), None, 50)
        .await
        .unwrap();
    assert!(
        history
            .iter()
            .any(|m| matches!(&m.content, Content::Text { text, .. } if text == "привет"))
    );

    // Deleted on this device only, found from a time a little off.
    let mine = history.iter().find(|m| m.outgoing).unwrap();
    b.client
        .delete_message(a.client.peer_id(), mine.msg_id, mine.sent_at_ms + 3)
        .await
        .unwrap();
    let left = b
        .client
        .history(a.client.peer_id(), None, 50)
        .await
        .unwrap();
    assert!(left.iter().all(|m| m.msg_id != mine.msg_id));
    assert_eq!(left.len(), history.len() - 1);
    b.client
        .delete_message(a.client.peer_id(), mine.msg_id, mine.sent_at_ms)
        .await
        .unwrap_err();
}

/// Sends a file; both sides remember where it is. Returns its id.
async fn send_a_file(a: &mut Peer, b: &mut Peer) -> FileId {
    let data = pattern(5 * 1024 * 1024 + 7);
    let src = a.dir.path().join("payload.bin");
    std::fs::write(&src, &data).unwrap();
    a.client
        .send_file(
            b.client.peer_id(),
            &src,
            "application/octet-stream",
            MediaKind::File,
        )
        .await
        .unwrap();
    let file_id = b
        .wait(|e| match e {
            Event::TransferOffered { file_id, name, .. } => {
                assert_eq!(name, "payload.bin");
                Some(*file_id)
            }
            _ => None,
        })
        .await;
    let dest = b.dir.path().join("received.bin");
    b.client.accept_file(file_id, dest.clone()).await.unwrap();
    b.transfer_complete(file_id).await;
    // Complete means closed on disk: the file can be opened at once.
    assert_eq!(std::fs::read(&dest).unwrap(), data);
    let location = |p: &Path| Some(p.to_str().unwrap().to_owned());
    assert_eq!(b.client.saved_file(file_id).await.unwrap(), location(&dest));
    a.transfer_complete(file_id).await;
    eventually("the sender remembers where the file is", async || {
        a.client.saved_file(file_id).await.unwrap() == location(&src)
    })
    .await;
    file_id
}

/// A staged copy (an Android pick) goes under its own name and is gone once
/// sent.
async fn send_a_staged_copy(a: &mut Peer, b: &mut Peer) {
    let staged = a.dir.path().join("pick-1234.tmp");
    std::fs::write(&staged, b"from a content provider").unwrap();
    let (_, staged_id) = a
        .client
        .send_staged_file(b.client.peer_id(), &staged, "photo.jpg", "image/jpeg")
        .await
        .unwrap();
    assert!(!staged.exists(), "the client took the copy over");
    let offered = b
        .wait(|e| match e {
            Event::TransferOffered {
                file_id,
                name,
                mime,
                ..
            } if *file_id == staged_id => Some((name.clone(), mime.clone())),
            _ => None,
        })
        .await;
    assert_eq!(offered, ("photo.jpg".to_owned(), "image/jpeg".to_owned()));
    let photo = b.dir.path().join("photo.jpg");
    b.client
        .accept_file(staged_id, photo.clone())
        .await
        .unwrap();
    b.transfer_complete(staged_id).await;
    assert_eq!(std::fs::read(&photo).unwrap(), b"from a content provider");
    a.transfer_complete(staged_id).await;
    let copy = a
        .dir
        .path()
        .join("outgoing")
        .join(format!("{}.bin", staged_id.to_hex()));
    eventually("a staged copy is neither kept nor remembered", async || {
        !copy.exists() && a.client.saved_file(staged_id).await.unwrap().is_none()
    })
    .await;
}

/// A platform that moves a received file (Android: into Downloads) says where.
async fn move_a_saved_file(b: &Peer, file_id: FileId) {
    let moved = "content://media/external/downloads/42".to_owned();
    b.client
        .move_saved_file(file_id, moved.clone())
        .await
        .unwrap();
    assert_eq!(b.client.saved_file(file_id).await.unwrap(), Some(moved));
}

async fn play_back_a_voice_note(a: &Peer, b: &mut Peer) {
    let voice = vec![3u8; 150 * 1024];
    let voice_len = u64::try_from(voice.len()).unwrap();
    let path = a.dir.path().join("voice.webm");
    std::fs::write(&path, &voice).unwrap();
    let kind = MediaKind::Voice {
        duration_ms: 9_000,
        waveform: vec![5; 64],
    };
    let (_, voice_id) = a
        .client
        .send_file(b.client.peer_id(), &path, "audio/webm", kind)
        .await
        .unwrap();
    b.transfer_complete(voice_id).await;
    eventually("both keep the same sealed copy", async || {
        let ours = std::fs::read(a.client.media_path(&voice_id));
        let theirs = std::fs::read(b.client.media_path(&voice_id));
        ours.is_ok_and(|ours| theirs.is_ok_and(|theirs| ours == theirs))
    })
    .await;
    let played = b.client.media_range(voice_id, 0, None).await.unwrap();
    assert_eq!(played.total, voice_len);
    assert_eq!(played.bytes, voice, "receiver plays the recording");
    let tail = a
        .client
        .media_range(voice_id, voice_len - 10, None)
        .await
        .unwrap();
    assert_eq!(
        (tail.start, tail.end, tail.bytes),
        (voice_len - 10, voice_len - 1, vec![3; 10])
    );
}

async fn stream_a_video_note(a: &mut Peer, b: &mut Peer) {
    let recording = pattern(300 * 1024);
    let (_, note_id) = a
        .client
        .send_media(
            b.client.peer_id(),
            recording.clone(),
            "note.webm",
            "video/webm",
            MediaKind::VideoNote {
                duration_ms: 4_000,
                poster: vec![0xFF, 0xD8],
            },
        )
        .await
        .unwrap();
    a.transfer_complete(note_id).await;
    // Complete means closed on disk: the note can be read back at once.
    b.transfer_complete(note_id).await;
    let mut got = Vec::new();
    while got.len() < recording.len() {
        let from = u64::try_from(got.len()).unwrap();
        let part = b.client.media_range(note_id, from, None).await.unwrap();
        got.extend_from_slice(&part.bytes);
    }
    assert_eq!(
        got, recording,
        "video note round-trips through ranged reads"
    );
    let staged = a
        .dir
        .path()
        .join("outgoing")
        .join(format!("{}.bin", note_id.to_hex()));
    eventually("staged plaintext is deleted once sent", async || {
        !staged.exists()
    })
    .await;
}

/// The server hands out its push key and takes a subscription, through the
/// onion relay (these clients require it).
async fn register_for_push(b: &mut Peer) {
    b.client.command(Command::EnablePush).await.unwrap();
    let key = b
        .wait(|e| match e {
            Event::PushKey { key } => Some(key.clone()),
            _ => None,
        })
        .await;
    assert_eq!(key.len(), 65);
    let register = Command::RegisterPush {
        endpoint: "https://push.invalid/up-b".into(),
        p256dh: *p256::SecretKey::random(&mut rand::rngs::OsRng)
            .public_key()
            .to_encoded_point(false)
            .as_bytes()
            .first_chunk::<65>()
            .unwrap(),
        auth: [1; 16],
    };
    b.client.command(register).await.unwrap();
    b.wait(|e| matches!(e, Event::PushRegistered).then_some(()))
        .await;
}

async fn deliver_offline_through_the_inbox(target: &Target, a: &mut Peer, b: Peer) {
    let b_id = b.client.peer_id();
    b.client.shutdown().await;
    // Stopped for good once `shutdown` returns, so the restart below cannot
    // overlap it on the same data (two would overwrite each other's keys).
    let late = b.client.send_text(a.client.peer_id(), "late".into(), None);
    assert!(matches!(late.await, Err(ClientError::Closed)));
    let (seed, dir) = (b.seed, b.dir);
    // The gateway drops B's session a moment after B left; until then a
    // message is still handed to it. Write again until one is left in B's
    // inbox.
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let sent = a
            .client
            .send_text(b_id, "пока тебя не было".into(), None)
            .await
            .unwrap();
        let queued = a
            .wait(|e| match e {
                Event::MessageStatus { msg_id, status } if *msg_id == sent => match status {
                    MessageStatus::Queued => Some(true),
                    MessageStatus::Sent => Some(false),
                    MessageStatus::Pending
                    | MessageStatus::Delivered
                    | MessageStatus::Read
                    | MessageStatus::Failed => None,
                },
                _ => None,
            })
            .await;
        if queued {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "B's session outlived {WAIT:?}"
        );
        tokio::time::sleep(POLL).await;
    }

    let mut b = Peer::start(target, seed, dir).await;
    assert_eq!(b.text_from_peer().await, "пока тебя не было");
    b.client.shutdown().await;
}
