use bytes::Bytes;
use cypher_common::Result;
use cypher_proto::Serializable;
use cypher_transport::FrameFlags;
use ed25519_dalek::VerifyingKey;
use rand::seq::SliceRandom;
use tracing::warn;

use super::bootstrap::SignedInboxResponse;
use super::decoder::decode_relay_response;
use super::encoder::encode_relay_request;
use super::jitter::pipeline_schedule;
use super::pool::{TransportHandle, TransportPool};

use std::sync::Arc;

/// Distributes inbox fetches across parallel transports with jitter and dummies.
pub struct PipelinedFetcher {
    pool: Arc<TransportPool>,
    inbox_verifying_key: VerifyingKey,
}

/// One unit of work: fetch a single inbox_id (real or dummy).
struct FetchJob {
    inbox_id: Vec<u8>,
    is_dummy: bool,
}

impl PipelinedFetcher {
    pub fn new(pool: Arc<TransportPool>, inbox_verifying_key: VerifyingKey) -> Self {
        Self {
            pool,
            inbox_verifying_key,
        }
    }

    /// Fetch all inbox_ids in parallel across available transports.
    ///
    /// Returns `(inbox_id, raw_proto_payload)` for each real inbox_id.
    /// Dummy fetches are discarded silently.
    pub async fn fetch_all(&self, inbox_ids: Vec<Vec<u8>>) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        if inbox_ids.is_empty() {
            return Ok(Vec::new());
        }

        // Build job list: real + dummy, shuffled.
        let mut jobs: Vec<FetchJob> = inbox_ids
            .iter()
            .cloned()
            .map(|id| FetchJob {
                inbox_id: id,
                is_dummy: false,
            })
            .collect();

        let dummy_count = 2.min(inbox_ids.len());
        for _ in 0..dummy_count {
            jobs.push(FetchJob {
                inbox_id: rand::random::<[u8; 32]>().to_vec(),
                is_dummy: true,
            });
        }
        jobs.shuffle(&mut rand::thread_rng());

        // Distribute across transports (target: 3 parallel).
        let transport_count = self.pool.relay_ready_count().await.clamp(1, 3);
        let chunks = distribute_jobs(jobs, transport_count);

        // Spawn parallel pipeline per transport chunk.
        let mut handles = Vec::with_capacity(chunks.len());
        for chunk in chunks {
            let pool = self.pool.clone();
            let inbox_verifying_key = self.inbox_verifying_key;
            handles.push(tokio::spawn(async move {
                pipeline_chunk(pool, inbox_verifying_key, chunk).await
            }));
        }

        // Collect results from all transports.
        let mut results = Vec::new();
        for handle in handles {
            match handle.await {
                Ok(Ok(chunk_results)) => results.extend(chunk_results),
                Ok(Err(e)) => warn!("pipeline chunk error: {e}"),
                Err(e) => warn!("pipeline task panicked: {e}"),
            }
        }

        Ok(results)
    }
}

/// Distribute jobs across N transport pipelines as evenly as possible.
fn distribute_jobs(jobs: Vec<FetchJob>, n: usize) -> Vec<Vec<FetchJob>> {
    let mut chunks: Vec<Vec<FetchJob>> = (0..n).map(|_| Vec::new()).collect();
    for (i, job) in jobs.into_iter().enumerate() {
        chunks[i % n].push(job);
    }
    chunks
}

/// Run a pipeline: acquire one transport, send all jobs with jitter, collect results.
async fn pipeline_chunk(
    pool: Arc<TransportPool>,
    inbox_verifying_key: VerifyingKey,
    jobs: Vec<FetchJob>,
) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
    let schedule = pipeline_schedule(jobs.len());
    let mut handle = pool.acquire().await?;
    let mut results = Vec::new();

    for (idx, job) in jobs.iter().enumerate() {
        tokio::time::sleep(schedule[idx]).await;

        match fetch_one(&mut handle, &job.inbox_id).await {
            Ok(response) => {
                let signed = SignedInboxResponse::parse(&response)?;
                signed.verify(&inbox_verifying_key, &job.inbox_id)?;

                // Send ACK only after we have verified the server signature.
                if let Some(token) = signed.claim_token.clone() {
                    if let Err(e) = send_ack(&mut handle, &job.inbox_id, token).await {
                        warn!("inbox ack failed: {e}");
                    }
                }

                if !job.is_dummy {
                    results.push((job.inbox_id.clone(), signed.proto_payload));
                }
            }
            Err(e) => {
                if !job.is_dummy {
                    warn!(inbox_id_len = job.inbox_id.len(), "fetch failed: {e}");
                }
            }
        }
    }

    pool.release(handle).await;
    Ok(results)
}

/// Fetch a single inbox_id using the given transport handle.
async fn fetch_one(handle: &mut TransportHandle, inbox_id: &[u8]) -> Result<Vec<u8>> {
    let fetch = cypher_proto::InboxFetch {
        inbox_id: inbox_id.to_vec(),
    };
    let fetch_payload = fetch.serialize();

    match handle {
        #[cfg(feature = "tor")]
        TransportHandle::Tor(session) => {
            let frame = session
                .send_and_recv(Bytes::from(fetch_payload), FrameFlags::NONE)
                .await?;
            Ok(frame.payload.to_vec())
        }
        TransportHandle::Relay { client, circuit } => {
            let onion_request = encode_relay_request(circuit, &fetch_payload)?;
            let onion_response = client.send_and_recv(onion_request).await?;
            decode_relay_response(&circuit.circuit_key, &circuit.circuit_id, &onion_response)
        }
        TransportHandle::Direct(session) => {
            let frame = session
                .send_and_recv(Bytes::from(fetch_payload), FrameFlags::NONE)
                .await?;
            Ok(frame.payload.to_vec())
        }
    }
}

/// Send InboxAck for two-phase fetch.
async fn send_ack(
    handle: &mut TransportHandle,
    inbox_id: &[u8],
    claim_token: Vec<u8>,
) -> Result<()> {
    let ack = cypher_proto::InboxAck {
        inbox_id: inbox_id.to_vec(),
        claim_token,
    };
    let ack_payload = ack.serialize();

    match handle {
        #[cfg(feature = "tor")]
        TransportHandle::Tor(session) => {
            let _ = session
                .send_and_recv(Bytes::from(ack_payload), FrameFlags::NONE)
                .await?;
        }
        TransportHandle::Relay { client, circuit } => {
            let onion = encode_relay_request(circuit, &ack_payload)?;
            let _ = client.send_and_recv(onion).await;
        }
        TransportHandle::Direct(session) => {
            let _ = session
                .send_and_recv(Bytes::from(ack_payload), FrameFlags::NONE)
                .await?;
        }
    }

    Ok(())
}
