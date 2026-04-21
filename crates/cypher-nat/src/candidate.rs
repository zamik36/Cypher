use std::net::SocketAddr;

/// ICE candidate type per RFC 8445.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateType {
    Host,
    ServerReflexive,
    Relay,
}

/// An ICE candidate with address, type, and priority.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub candidate_type: CandidateType,
    pub addr: SocketAddr,
    pub priority: u32,
}

impl Candidate {
    pub fn host(addr: SocketAddr) -> Self {
        Self {
            candidate_type: CandidateType::Host,
            addr,
            priority: 300,
        }
    }

    pub fn server_reflexive(addr: SocketAddr) -> Self {
        Self {
            candidate_type: CandidateType::ServerReflexive,
            addr,
            priority: 200,
        }
    }

    pub fn relay(addr: SocketAddr) -> Self {
        Self {
            candidate_type: CandidateType::Relay,
            addr,
            priority: 100,
        }
    }
}

impl PartialEq for Candidate {
    fn eq(&self, other: &Self) -> bool {
        self.candidate_type == other.candidate_type && self.addr == other.addr
    }
}

impl Eq for Candidate {}

/// Sort candidates by priority in descending order (host > srflx > relay).
pub fn sort_candidates(candidates: &mut [Candidate]) {
    candidates.sort_by_key(|c| std::cmp::Reverse(c.priority));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_priority_ordering() {
        let host = Candidate::host("127.0.0.1:1234".parse().unwrap());
        let srflx = Candidate::server_reflexive("1.2.3.4:5678".parse().unwrap());
        let relay = Candidate::relay("5.6.7.8:9012".parse().unwrap());

        let mut candidates = vec![relay.clone(), host.clone(), srflx.clone()];
        sort_candidates(&mut candidates);

        assert_eq!(candidates[0].candidate_type, CandidateType::Host);
        assert_eq!(candidates[1].candidate_type, CandidateType::ServerReflexive);
        assert_eq!(candidates[2].candidate_type, CandidateType::Relay);
    }
}
