use std::sync::Arc;

/// Anonymity level of the current inbox fetch transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AnonymityLevel {
    /// Level 0: direct fetch.
    Direct = 0,
    /// Level 2: 1-hop relay.
    Relay = 2,
    /// Level 3: Tor transport.
    Tor = 3,
}

impl AnonymityLevel {
    pub fn description(&self) -> &'static str {
        match self {
            Self::Direct => "Direct fetch",
            Self::Relay => "Inbox fetch routed through relay",
            Self::Tor => "Inbox fetch routed through Tor",
        }
    }
}

impl std::fmt::Display for AnonymityLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Direct => "Direct",
            Self::Relay => "Relay",
            Self::Tor => "Tor",
        };
        write!(f, "Level {} ({name})", *self as u8)
    }
}

pub async fn compute_level(pool: &Arc<crate::onion::pool::TransportPool>) -> AnonymityLevel {
    #[cfg(feature = "tor")]
    if pool.tor_ready_count().await > 0 {
        return AnonymityLevel::Tor;
    }

    if pool.relay_ready_count().await > 0 {
        AnonymityLevel::Relay
    } else {
        AnonymityLevel::Direct
    }
}
