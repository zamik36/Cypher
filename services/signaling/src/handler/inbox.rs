use bytes::Bytes;
use cypher_types::Addr;
use cypher_wire::{DeliveryStatus, ErrorCode, MAX_INBOX_BATCH, ServerMsg, access_hash};
use rand::RngCore;

use crate::store::PutOutcome;

use super::{Delivery, HOUR_SECS, Handler, SEALED_PER_TOKEN_PER_HOUR, error};

impl Handler {
    pub(super) async fn inbox_put(
        &self,
        inbox: &[u8; 32],
        item: &[u8],
    ) -> redis::RedisResult<ServerMsg> {
        Ok(
            match self.store.inbox_put(inbox, item, crate::now_secs()).await? {
                PutOutcome::Stored => {
                    self.signal(inbox).await?;
                    ServerMsg::Done
                }
                PutOutcome::Full => error(ErrorCode::TooLarge),
                // To the writer as if this inbox were full: it tries again later.
                PutOutcome::OverBudget => {
                    self.inbox_quota_refused.inc();
                    error(ErrorCode::TooLarge)
                }
                PutOutcome::Inactive => error(ErrorCode::NotFound),
            },
        )
    }

    /// Hands `item` to the device's gateway, which takes it only with a
    /// token the device told it; a device that is away finds it in `inbox`.
    pub(super) async fn send_sealed(
        &self,
        to: Addr,
        token: &[u8; 32],
        inbox: Option<[u8; 32]>,
        item: &Bytes,
    ) -> redis::RedisResult<ServerMsg> {
        let bucket = [b"ss:".as_slice(), &access_hash(token)].concat();
        if !self
            .store
            .admit(&bucket, SEALED_PER_TOKEN_PER_HOUR, HOUR_SECS)
            .await?
        {
            return Ok(error(ErrorCode::RateLimited));
        }
        let payload = Bytes::from([token.as_slice(), item].concat());
        let status = match self.courier.deliver(to, payload).await {
            Delivery::Answered(cypher_server_kit::SEALED_REFUSED) => {
                return Ok(error(ErrorCode::Unauthorized));
            }
            Delivery::Answered(status) if status == DeliveryStatus::Delivered as u8 => {
                DeliveryStatus::Delivered
            }
            Delivery::Answered(_) => DeliveryStatus::Busy,
            // The way to the gateway is broken, not the device busy: the
            // client sends as itself for a while instead of waiting.
            Delivery::Unavailable => {
                self.sealed_unavailable.inc();
                return Ok(error(ErrorCode::Unavailable));
            }
            Delivery::Away => match inbox {
                Some(inbox) => {
                    return Ok(match self.inbox_put(&inbox, item).await? {
                        ServerMsg::Done => ServerMsg::SendAck {
                            status: DeliveryStatus::Queued,
                        },
                        refused => refused,
                    });
                }
                None => DeliveryStatus::Offline,
            },
        };
        Ok(ServerMsg::SendAck { status })
    }

    pub(super) async fn inbox_fetch(
        &self,
        inbox: &[u8; 32],
        close: bool,
    ) -> redis::RedisResult<ServerMsg> {
        let Ok(_permit) = self.fetches.try_acquire() else {
            return Ok(error(ErrorCode::Unavailable));
        };
        let mut claim = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut claim);
        let items = self
            .store
            .inbox_fetch(inbox, &claim, MAX_INBOX_BATCH, close, crate::now_secs())
            .await?;
        // All inboxes together are full: the owner asks again later, and
        // its inbox is not open to writers meanwhile.
        Ok(items.map_or_else(
            || {
                self.inbox_quota_refused.inc();
                error(ErrorCode::Unavailable)
            },
            |items| ServerMsg::InboxBatch { claim, items },
        ))
    }
}
