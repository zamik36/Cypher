use std::env;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context};
use bytes::Bytes;
use cypher_proto::{Serializable, SessionAck};
use cypher_transport::{FrameFlags, TransportSession};
use rand::RngCore;
use redis::AsyncCommands;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct StoredSession {
    session_id: u64,
}

struct TestClient {
    session: TransportSession,
}

impl TestClient {
    async fn connect(peer_id: &[u8]) -> anyhow::Result<Self> {
        let tls_config = cypher_tls::make_client_config_insecure();
        let mut session = TransportSession::connect(&gateway_addr(), tls_config).await?;
        let init = cypher_proto::SessionInit {
            client_id: peer_id.to_vec(),
            nonce: vec![0u8; 32],
        };
        session
            .send_frame(Bytes::from(init.serialize()), FrameFlags::SESSION_INIT)
            .await?;

        let ack = session.recv_frame().await?;
        SessionAck::deserialize(&ack.payload)
            .map_err(|error| anyhow!("invalid SESSION_ACK: {error}"))?;

        Ok(Self { session })
    }

    async fn upload_prekeys(&mut self, inbox_id: &[u8]) -> anyhow::Result<()> {
        let msg = cypher_proto::KeysUploadPrekeys {
            identity_key: vec![0x11; 32],
            signed_prekey: vec![0x22; 32],
            inbox_id: inbox_id.to_vec(),
        };
        self.session
            .send_frame(Bytes::from(msg.serialize()), FrameFlags::NONE)
            .await?;
        Ok(())
    }

    async fn send_chat(&mut self, target_peer: &[u8]) -> anyhow::Result<()> {
        let msg = cypher_proto::ChatSend {
            peer_id: target_peer.to_vec(),
            ciphertext: vec![0xAA, 0xBB, 0xCC],
            ratchet_key: vec![0x33; 32],
            msg_no: 1,
        };
        self.session
            .send_frame(Bytes::from(msg.serialize()), FrameFlags::NONE)
            .await?;
        Ok(())
    }

    async fn close(&mut self) -> anyhow::Result<()> {
        self.session.close().await?;
        Ok(())
    }
}

fn gateway_addr() -> String {
    env::var("P2P_TEST_GATEWAY_ADDR").unwrap_or_else(|_| "127.0.0.1:9100".to_string())
}

fn redis_url() -> String {
    env::var("P2P_TEST_REDIS_URL").expect("P2P_TEST_REDIS_URL must be set for integration tests")
}

fn random_bytes() -> [u8; 32] {
    let mut out = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut out);
    out
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

async fn redis_conn() -> anyhow::Result<redis::aio::MultiplexedConnection> {
    let client = redis::Client::open(redis_url()).context("open redis client")?;
    client
        .get_multiplexed_async_connection()
        .await
        .context("connect to redis")
}

async fn load_session(peer_hex: &str) -> anyhow::Result<Option<StoredSession>> {
    let mut redis = redis_conn().await?;
    let key = format!("peer:{peer_hex}:session");
    let value: Option<String> = redis.get(&key).await?;
    value
        .map(|json| serde_json::from_str(&json).context("decode stored peer session"))
        .transpose()
}

async fn reverse_mapping(session_id: u64) -> anyhow::Result<Option<String>> {
    let mut redis = redis_conn().await?;
    let key = format!("session:{session_id}:peer");
    redis.get(&key).await.map_err(Into::into)
}

async fn inbox_len(inbox_hex: &str) -> anyhow::Result<usize> {
    let mut redis = redis_conn().await?;
    let key = format!("inbox:{inbox_hex}");
    redis.llen(&key).await.map_err(Into::into)
}

async fn delete_key(key: &str) -> anyhow::Result<()> {
    let mut redis = redis_conn().await?;
    redis.del::<_, ()>(key).await?;
    Ok(())
}

async fn wait_until<F, Fut>(label: &str, timeout: Duration, mut check: F) -> anyhow::Result<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<bool>>,
{
    let deadline = Instant::now() + timeout;
    loop {
        if check().await? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(anyhow!("timed out waiting for {label}"));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
#[ignore = "manual compose regression against local docker stack"]
async fn reconnect_cleanup_and_offline_inbox_flow() -> anyhow::Result<()> {
    let peer_a = random_bytes();
    let peer_b = random_bytes();
    let inbox_b = random_bytes();

    let peer_a_hex = hex_encode(&peer_a);
    let peer_b_hex = hex_encode(&peer_b);
    let inbox_b_hex = hex_encode(&inbox_b);

    delete_key(&format!("peer:{peer_a_hex}:session")).await?;
    delete_key(&format!("peer:{peer_b_hex}:session")).await?;
    delete_key(&format!("inbox:{inbox_b_hex}")).await?;

    let mut b1 = TestClient::connect(&peer_b).await?;
    b1.upload_prekeys(&inbox_b).await?;

    wait_until("initial peer session", Duration::from_secs(5), || async {
        Ok(load_session(&peer_b_hex).await?.is_some())
    })
    .await?;

    let first_session = load_session(&peer_b_hex)
        .await?
        .context("missing first session")?
        .session_id;
    assert_eq!(
        reverse_mapping(first_session).await?,
        Some(peer_b_hex.clone())
    );

    let mut b2 = TestClient::connect(&peer_b).await?;
    b2.upload_prekeys(&inbox_b).await?;

    wait_until(
        "replacement peer session",
        Duration::from_secs(5),
        || async {
            Ok(matches!(
                load_session(&peer_b_hex).await?,
                Some(StoredSession { session_id }) if session_id != first_session
            ))
        },
    )
    .await?;

    let second_session = load_session(&peer_b_hex)
        .await?
        .context("missing second session")?
        .session_id;
    assert_ne!(first_session, second_session);

    wait_until(
        "old reverse mapping removal",
        Duration::from_secs(5),
        || async { Ok(reverse_mapping(first_session).await?.is_none()) },
    )
    .await?;
    assert_eq!(
        reverse_mapping(second_session).await?,
        Some(peer_b_hex.clone())
    );

    b1.close().await?;

    wait_until(
        "stale deregister preserving newest session",
        Duration::from_secs(5),
        || async {
            Ok(matches!(
                load_session(&peer_b_hex).await?,
                Some(StoredSession { session_id }) if session_id == second_session
            ) && reverse_mapping(second_session).await? == Some(peer_b_hex.clone()))
        },
    )
    .await?;

    let mut a = TestClient::connect(&peer_a).await?;

    b2.close().await?;

    wait_until("final peer deregister", Duration::from_secs(5), || async {
        Ok(load_session(&peer_b_hex).await?.is_none()
            && reverse_mapping(second_session).await?.is_none())
    })
    .await?;

    a.send_chat(&peer_b).await?;

    wait_until("offline inbox storage", Duration::from_secs(5), || async {
        Ok(inbox_len(&inbox_b_hex).await? >= 1)
    })
    .await?;

    a.close().await?;
    Ok(())
}
