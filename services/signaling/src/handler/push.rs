use cypher_crypto::inbox::{InboxOp, push_extra};
use cypher_wire::{ClientMsg, ErrorCode, ServerMsg};
use tracing::warn;

use crate::push::{Outcome, Subscription};

use super::{Caller, Handler, PUSH_INTERVAL_SECS, error, proven};

impl Handler {
    pub(super) async fn push_request(
        &self,
        caller: Caller,
        msg: ClientMsg,
    ) -> redis::RedisResult<ServerMsg> {
        match (msg, caller) {
            (ClientMsg::PushKey, _) => Ok(self.push_key()),
            (
                ClientMsg::PushRegister {
                    proof,
                    endpoint,
                    p256dh,
                    auth,
                },
                Caller::Onion,
            ) => {
                let extra = push_extra(&endpoint, &p256dh, &auth);
                if !proven(&proof, InboxOp::PushRegister, &extra) {
                    return Ok(error(ErrorCode::Unauthorized));
                }
                let sub = Subscription {
                    endpoint,
                    p256dh,
                    auth,
                };
                self.push_register(&proof.inbox, &sub).await
            }
            (ClientMsg::PushUnregister { proof }, Caller::Onion) => {
                if !proven(&proof, InboxOp::PushUnregister, &[]) {
                    return Ok(error(ErrorCode::Unauthorized));
                }
                self.store
                    .push_unregister(&proof.inbox)
                    .await
                    .map(|()| ServerMsg::Done)
            }
            // Registering only anonymously: a session would tie the
            // device's push address to its identity.
            (
                ClientMsg::PushRegister { .. } | ClientMsg::PushUnregister { .. },
                Caller::Session(_),
            )
            | (
                ClientMsg::Hello { .. }
                | ClientMsg::Auth { .. }
                | ClientMsg::Ping
                | ClientMsg::Send { .. }
                | ClientMsg::SendSealed { .. }
                | ClientMsg::SetAccess { .. }
                | ClientMsg::PublishKeys { .. }
                | ClientMsg::FetchKeys { .. }
                | ClientMsg::PublishDevices { .. }
                | ClientMsg::FetchDevices { .. }
                | ClientMsg::CreateLink
                | ClientMsg::ResolveLink { .. }
                | ClientMsg::InboxPut { .. }
                | ClientMsg::InboxFetch { .. }
                | ClientMsg::InboxAck { .. }
                | ClientMsg::Bootstrap,
                _,
            ) => Ok(error(ErrorCode::BadRequest)),
        }
    }

    fn push_key(&self) -> ServerMsg {
        self.push.as_ref().map_or_else(
            || error(ErrorCode::Unavailable),
            |push| ServerMsg::PushKey {
                key: push.public_key(),
            },
        )
    }

    async fn push_register(
        &self,
        inbox: &[u8; 32],
        sub: &Subscription,
    ) -> redis::RedisResult<ServerMsg> {
        let Some(push) = &self.push else {
            return Ok(error(ErrorCode::Unavailable));
        };
        if !push.accepts(&sub.endpoint) || p256::PublicKey::from_sec1_bytes(&sub.p256dh).is_err() {
            return Ok(error(ErrorCode::BadRequest));
        }
        self.store.push_register(inbox, &sub.to_bytes()).await?;
        Ok(ServerMsg::Done)
    }

    /// The inbox got an item: tell its owner's device, at most once per
    /// [`PUSH_INTERVAL_SECS`], without holding up the reply. With
    /// [`super::PUSH_PERMITS`] signals on their way already, this one is dropped.
    pub(super) async fn signal(&self, inbox: &[u8; 32]) -> redis::RedisResult<()> {
        let Some(push) = self.push.clone() else {
            return Ok(());
        };
        let Some(sub) = self
            .store
            .push_subscription(inbox)
            .await?
            .and_then(|raw| Subscription::from_bytes(&raw))
        else {
            return Ok(());
        };
        // Before the interval is used up, so a dropped signal does not
        // hold back the next one.
        let Ok(permit) = std::sync::Arc::clone(&self.pushes).try_acquire_owned() else {
            self.push_shed.inc();
            return Ok(());
        };
        let bucket = [&b"pn:"[..], inbox].concat();
        if !self.store.admit(&bucket, 1, PUSH_INTERVAL_SECS).await? {
            return Ok(());
        }
        let (store, inbox) = (self.store.clone(), *inbox);
        tokio::spawn(async move {
            if push.send(&sub).await == Outcome::Gone
                && let Err(e) = store.push_unregister(&inbox).await
            {
                warn!("forgetting a push subscription failed: {e}");
            }
            drop(permit);
        });
        Ok(())
    }
}
