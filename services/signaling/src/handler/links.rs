use cypher_types::{LinkId, PeerId};
use cypher_wire::{ErrorCode, ServerMsg};

use super::{
    HOUR_SECS, Handler, LINKS_PER_HOUR, RESOLVES_PER_HOUR, RESOLVES_PER_LINK_PER_HOUR, error,
};

impl Handler {
    pub(super) async fn create_link(&self, peer: &PeerId) -> redis::RedisResult<ServerMsg> {
        if !self.admit(b"cl:", peer, LINKS_PER_HOUR).await? {
            return Ok(error(ErrorCode::RateLimited));
        }
        let link = LinkId::random(&mut rand::rngs::OsRng);
        Ok(if self.store.create_link(&link, peer).await? {
            ServerMsg::LinkCreated { link }
        } else {
            error(ErrorCode::Internal)
        })
    }

    /// Who made the invite `link`. Bounded per invite, and per identity when
    /// one asks.
    pub(super) async fn resolve_link(
        &self,
        requester: Option<&PeerId>,
        link: &LinkId,
    ) -> redis::RedisResult<ServerMsg> {
        if let Some(peer) = requester
            && !self.admit(b"rq:", peer, RESOLVES_PER_HOUR).await?
        {
            return Ok(error(ErrorCode::RateLimited));
        }
        let bucket = [b"rl:".as_slice(), link.as_str().as_bytes()].concat();
        if !self
            .store
            .admit(&bucket, RESOLVES_PER_LINK_PER_HOUR, HOUR_SECS)
            .await?
        {
            return Ok(error(ErrorCode::RateLimited));
        }
        Ok(match self.store.resolve_link(link).await? {
            Some(peer) => ServerMsg::LinkResolved { peer },
            None => error(ErrorCode::NotFound),
        })
    }
}
