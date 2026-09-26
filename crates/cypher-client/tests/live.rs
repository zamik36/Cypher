//! End-to-end against a running stack (gateway, signaling, relay, NATS,
//! Redis). Enabled by `CYPHER_LIVE_GATEWAY=host:port` and
//! `CYPHER_LIVE_CA=<pem bundle pinning gateway and relay>`.

use std::path::Path;
use std::time::Duration;

use cypher_client::{Client, Config, Content};
use cypher_core::{Command, Event, MediaKind, MessageStatus};
use cypher_crypto::IdentitySeed;
use tokio::sync::mpsc::UnboundedReceiver;

const WAIT: Duration = Duration::from_secs(20);

fn live_config(dir: &Path) -> Option<Config> {
    let gateway = std::env::var("CYPHER_LIVE_GATEWAY").ok()?;
    let pem = std::fs::read_to_string(std::env::var("CYPHER_LIVE_CA").ok()?).ok()?;
    Some(Config {
        gateway_addr: gateway,
        tls: cypher_tls::make_client_config_with_pem(&pem).ok()?,
        data_dir: dir.to_owned(),
        require_onion: true,
    })
}

async fn wait_for<T>(
    events: &mut UnboundedReceiver<Event>,
    mut pick: impl FnMut(&Event) -> Option<T>,
) -> T {
    tokio::time::timeout(WAIT, async {
        loop {
            let event = events.recv().await.expect("client running");
            if let Some(v) = pick(&event) {
                return v;
            }
        }
    })
    .await
    .expect("event within timeout")
}

async fn connected(seed: &IdentitySeed, dir: &Path) -> (Client, UnboundedReceiver<Event>) {
    let (client, mut events) = Client::start(seed, live_config(dir).unwrap())
        .await
        .unwrap();
    wait_for(&mut events, |e| matches!(e, Event::Connected).then_some(())).await;
    (client, events)
}

#[tokio::test(flavor = "multi_thread")]
async fn full_user_journey_against_live_stack() {
    let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    if live_config(dir_a.path()).is_none() {
        eprintln!("CYPHER_LIVE_* not set; skipping");
        return;
    }
    let (seed_a, seed_b) = (IdentitySeed::generate(), IdentitySeed::generate());
    let (a, mut ev_a) = connected(&seed_a, dir_a.path()).await;
    let (b, mut ev_b) = connected(&seed_b, dir_b.path()).await;

    a.command(Command::CreateLink).await.unwrap();
    let link = wait_for(&mut ev_a, |e| match e {
        Event::LinkCreated { link } => Some(link.clone()),
        _ => None,
    })
    .await;
    b.command(Command::JoinLink { link }).await.unwrap();
    wait_for(&mut ev_b, |e| {
        matches!(e, Event::PeerAdded { .. }).then_some(())
    })
    .await;
    wait_for(&mut ev_a, |e| {
        matches!(e, Event::PeerAdded { .. }).then_some(())
    })
    .await;

    let hi = a.send_text(b.peer_id(), "привет".into()).await.unwrap();
    let text = wait_for(&mut ev_b, |e| match e {
        Event::Message(m) if !m.outgoing => match &m.content {
            Content::Text { text, .. } => Some(text.clone()),
            _ => None,
        },
        _ => None,
    })
    .await;
    assert_eq!(text, "привет");
    wait_for(&mut ev_a, |e| match e {
        Event::MessageStatus {
            msg_id,
            status: MessageStatus::Delivered,
        } if *msg_id == hi => Some(()),
        _ => None,
    })
    .await;
    b.send_text(a.peer_id(), "hi back".into()).await.unwrap();
    wait_for(&mut ev_a, |e| {
        matches!(e, Event::Message(m) if !m.outgoing).then_some(())
    })
    .await;

    let data: Vec<u8> = (0..5 * 1024 * 1024 + 7).map(|i| (i % 251) as u8).collect();
    let src = dir_a.path().join("payload.bin");
    std::fs::write(&src, &data).unwrap();
    a.send_file(
        b.peer_id(),
        &src,
        "application/octet-stream",
        MediaKind::File,
    )
    .await
    .unwrap();
    let file_id = wait_for(&mut ev_b, |e| match e {
        Event::TransferOffered { file_id, name, .. } => {
            assert_eq!(name, "payload.bin");
            Some(*file_id)
        }
        _ => None,
    })
    .await;
    let dest = dir_b.path().join("received.bin");
    b.accept_file(file_id, dest.clone()).await.unwrap();
    wait_for(&mut ev_b, |e| {
        matches!(e, Event::TransferComplete { file_id: f } if *f == file_id).then_some(())
    })
    .await;
    wait_for(&mut ev_a, |e| {
        matches!(e, Event::TransferComplete { file_id: f } if *f == file_id).then_some(())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(std::fs::read(&dest).unwrap(), data);

    let voice = dir_a.path().join("voice.webm");
    std::fs::write(&voice, vec![3u8; 150 * 1024]).unwrap();
    let kind = MediaKind::Voice {
        duration_ms: 9_000,
        waveform: vec![5; 64],
    };
    let (_, voice_id) = a
        .send_file(b.peer_id(), &voice, "audio/webm", kind)
        .await
        .unwrap();
    wait_for(&mut ev_b, |e| {
        matches!(e, Event::TransferComplete { file_id: f } if *f == voice_id).then_some(())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        std::fs::read(a.media_path(&voice_id)).unwrap(),
        std::fs::read(b.media_path(&voice_id)).unwrap(),
    );

    let history = b.history(a.peer_id(), None, 50).await.unwrap();
    assert!(
        history
            .iter()
            .any(|m| matches!(&m.content, Content::Text { text, .. } if text == "привет"))
    );

    b.shutdown().await;
    drop(b);
    drop(ev_b);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let queued = a
        .send_text(
            seed_b.derive_identity().peer_id(),
            "пока тебя не было".into(),
        )
        .await
        .unwrap();
    wait_for(&mut ev_a, |e| match e {
        Event::MessageStatus {
            msg_id,
            status: MessageStatus::Queued,
        } if *msg_id == queued => Some(()),
        _ => None,
    })
    .await;

    let (_b, mut ev_b) = connected(&seed_b, dir_b.path()).await;
    let offline = wait_for(&mut ev_b, |e| match e {
        Event::Message(m) if !m.outgoing => match &m.content {
            Content::Text { text, .. } if text == "пока тебя не было" => Some(()),
            _ => None,
        },
        _ => None,
    })
    .await;
    assert_eq!(offline, ());
    a.shutdown().await;
}
