#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers fail loudly on broken fixtures"
)]

//! End-to-end against a running stack (gateway, signaling, relay, NATS,
//! Redis). Enabled by `CYPHER_LIVE_GATEWAY=host:port` and
//! `CYPHER_LIVE_CA=<pem bundle pinning gateway and relay>`.

use std::path::Path;
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

fn live_config(dir: &Path) -> Option<Config> {
    let gateway = std::env::var("CYPHER_LIVE_GATEWAY").ok()?;
    let pem = std::fs::read_to_string(std::env::var("CYPHER_LIVE_CA").ok()?).ok()?;
    Some(Config {
        gateway_addr: gateway,
        tls: cypher_tls::make_client_config_with_pem(&pem).ok()?,
        data_dir: dir.to_owned(),
        require_onion: true,
        tor: None,
    })
}

/// A running client with its identity, event stream and data directory.
struct Peer {
    seed: IdentitySeed,
    dir: TempDir,
    client: Client,
    events: UnboundedReceiver<Event>,
}

impl Peer {
    async fn start(seed: IdentitySeed, dir: TempDir) -> Self {
        let (client, events) = Client::start(&seed, live_config(dir.path()).unwrap())
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
        peer
    }

    async fn wait<T>(&mut self, mut pick: impl FnMut(&Event) -> Option<T>) -> T {
        tokio::time::timeout(WAIT, async {
            loop {
                let event = self.events.recv().await.expect("client running");
                if let Some(v) = pick(&event) {
                    return v;
                }
            }
        })
        .await
        .expect("event within timeout")
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

#[tokio::test(flavor = "multi_thread")]
async fn full_user_journey_against_live_stack() {
    let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    if live_config(dir_a.path()).is_none() {
        eprintln!("CYPHER_LIVE_* not set; skipping");
        return;
    }
    let mut a = Peer::start(IdentitySeed::generate(), dir_a).await;
    let mut b = Peer::start(IdentitySeed::generate(), dir_b).await;

    pair_through_a_link(&mut a, &mut b).await;
    chat_both_ways(&mut a, &mut b).await;
    send_a_file(&mut a, &mut b).await;
    play_back_a_voice_note(&a, &mut b).await;
    stream_a_video_note(&mut a, &b).await;
    deliver_offline_through_the_inbox(&mut a, b).await;
    a.client.shutdown().await;
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
        .send_text(b.client.peer_id(), "привет".into())
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
    b.client
        .send_text(a.client.peer_id(), "hi back".into())
        .await
        .unwrap();
    assert_eq!(a.text_from_peer().await, "hi back");

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
}

async fn send_a_file(a: &mut Peer, b: &mut Peer) {
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
    a.transfer_complete(file_id).await;
    tokio::time::sleep(SETTLE).await;
    assert_eq!(std::fs::read(&dest).unwrap(), data);
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

async fn deliver_offline_through_the_inbox(a: &mut Peer, b: Peer) {
    let b_id = b.client.peer_id();
    b.client.shutdown().await;
    let (seed, dir) = (b.seed, b.dir);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let queued = a
        .client
        .send_text(b_id, "пока тебя не было".into())
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

    let mut b = Peer::start(seed, dir).await;
    assert_eq!(b.text_from_peer().await, "пока тебя не было");
    b.client.shutdown().await;
}
