//! The user journey: pair through a link, chat, send a file, voice and video
//! notes, then deliver while the recipient is offline.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use cypher_client::{Client, Config, Content};
use cypher_core::{Command, Event, MediaKind, MessageStatus};
use cypher_crypto::IdentitySeed;
use cypher_types::FileId;
use tempfile::TempDir;
use tokio::sync::mpsc::UnboundedReceiver;

const WAIT: Duration = Duration::from_secs(20);
/// Lets the IO thread flush a finished sink before the file is read back.
const SETTLE: Duration = Duration::from_millis(200);

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
    stream_a_video_note(&mut a, &b).await;
    deliver_offline_through_the_inbox(target, &mut a, b).await;
    a.client.shutdown().await;
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
        let (client, events) = Client::start(&seed, target.client_config(dir.path()))
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
    a.client.command(Command::CreateLink).await.unwrap();
    let link = a
        .wait(|e| match e {
            Event::LinkCreated { link } => Some(link.clone()),
            _ => None,
        })
        .await;
    b.client.command(Command::JoinLink { link }).await.unwrap();
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
    tokio::time::sleep(SETTLE).await;
    assert_eq!(a.client.saved_file(file_id).await.unwrap(), location(&src));
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
    tokio::time::sleep(SETTLE).await;
    let copy = a
        .dir
        .path()
        .join("outgoing")
        .join(format!("{}.bin", staged_id.to_hex()));
    assert!(
        !copy.exists() && a.client.saved_file(staged_id).await.unwrap().is_none(),
        "a staged copy is neither kept nor remembered"
    );
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
    tokio::time::sleep(SETTLE).await;
    assert_eq!(
        std::fs::read(a.client.media_path(&voice_id)).unwrap(),
        std::fs::read(b.client.media_path(&voice_id)).unwrap(),
    );
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

async fn stream_a_video_note(a: &mut Peer, b: &Peer) {
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
    tokio::time::sleep(SETTLE).await;
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
    assert!(!staged.exists(), "staged plaintext is deleted once sent");
}

async fn deliver_offline_through_the_inbox(target: &Target, a: &mut Peer, b: Peer) {
    let b_id = b.client.peer_id();
    b.client.shutdown().await;
    let (seed, dir) = (b.seed, b.dir);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let queued = a
        .client
        .send_text(b_id, "пока тебя не было".into(), None)
        .await
        .unwrap();
    a.wait(|e| match e {
        Event::MessageStatus {
            msg_id,
            status: MessageStatus::Queued,
        } if *msg_id == queued => Some(()),
        _ => None,
    })
    .await;

    let mut b = Peer::start(target, seed, dir).await;
    assert_eq!(b.text_from_peer().await, "пока тебя не было");
    b.client.shutdown().await;
}
