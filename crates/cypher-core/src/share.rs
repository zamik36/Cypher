//! What a user hands out to be found: `<link id>-<fingerprint>`. The server
//! only ever sees the link id and answers it with a peer; the fingerprint of
//! that peer's identity, which the server never gets, lets the joiner reject
//! an answer that is not the host who made the link.

use std::fmt;

use cypher_crypto::fingerprint::link_fingerprint;
use cypher_types::{LinkId, PeerId, base32};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareLink {
    link: LinkId,
    fingerprint: String,
}

impl ShareLink {
    pub fn new(link: LinkId, host: &PeerId) -> Self {
        Self {
            link,
            fingerprint: fingerprint_of(host),
        }
    }

    /// Accepts only well-formed links; surrounding whitespace is ignored.
    pub fn parse(s: &str) -> Option<Self> {
        let (link, fingerprint) = s.trim().split_once('-')?;
        let well_formed = fingerprint.len() == LinkId::ENCODED_LEN
            && fingerprint
                .bytes()
                .all(|c| matches!(c, b'a'..=b'z' | b'2'..=b'7'));
        if !well_formed {
            return None;
        }
        Some(Self {
            link: LinkId::parse(link)?,
            fingerprint: fingerprint.to_owned(),
        })
    }

    /// The part the server resolves.
    pub fn link(&self) -> &LinkId {
        &self.link
    }

    /// Whether `peer` is the identity this link was made for.
    pub fn is_host(&self, peer: &PeerId) -> bool {
        self.fingerprint == fingerprint_of(peer)
    }
}

impl fmt::Display for ShareLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.link, self.fingerprint)
    }
}

fn fingerprint_of(peer: &PeerId) -> String {
    base32(&link_fingerprint(peer.as_bytes()))
}

#[cfg(test)]
mod tests {
    use rand::rngs::OsRng;

    use super::*;

    #[test]
    fn a_link_names_exactly_its_host() {
        let host = PeerId([1; 32]);
        let shared = ShareLink::new(LinkId::random(&mut OsRng), &host);
        let parsed = ShareLink::parse(&format!("  {shared}\n")).unwrap();
        assert_eq!(parsed, shared);
        assert!(parsed.is_host(&host));
        assert!(!parsed.is_host(&PeerId([2; 32])), "another key is rejected");
    }

    #[test]
    fn malformed_links_are_rejected() {
        let link = LinkId::random(&mut OsRng);
        for bad in [
            link.to_string(),
            format!("{link}-"),
            format!("{link}-short"),
            format!("{link}-{}", "A".repeat(26)),
            format!("bad-{}", "a".repeat(26)),
        ] {
            assert_eq!(ShareLink::parse(&bad), None, "{bad}");
        }
    }
}
