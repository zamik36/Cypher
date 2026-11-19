//! Short, comparable digests of identity keys: one committed to in share
//! links, one people read to each other to verify a contact.

use std::fmt::Write as _;

use sha2::{Digest, Sha256, Sha512};

/// Bytes of an identity a share link commits to. At 128 bits a server
/// answering the link with its own key would need a second preimage.
pub const LINK_FINGERPRINT_LEN: usize = 16;

/// Digits of a safety number: 30 per party, read in groups of five.
pub const SAFETY_NUMBER_DIGITS: usize = 60;

const SAFETY_ITERATIONS: usize = 5200;
const DIGIT_GROUPS_PER_PARTY: usize = 6;
const GROUP_BYTES: usize = 5;
const GROUP_MODULUS: u64 = 100_000;

/// The digest of `identity` that share links carry next to the link id.
pub fn link_fingerprint(identity: &[u8; 32]) -> [u8; LINK_FINGERPRINT_LEN] {
    let digest = Sha256::new()
        .chain_update(b"cypher/v2/link-fingerprint")
        .chain_update(identity)
        .finalize();
    let mut out = [0u8; LINK_FINGERPRINT_LEN];
    if let Some(head) = digest.first_chunk::<LINK_FINGERPRINT_LEN>() {
        out = *head;
    }
    out
}

/// The 60-digit number two contacts compare out of band; it is the same
/// on both sides. Each party's half is an iterated hash of its identity, so
/// finding a key that matches a given half costs ~2^100 hash rounds.
pub fn safety_number(a: &[u8; 32], b: &[u8; 32]) -> String {
    let (first, second) = if a <= b { (a, b) } else { (b, a) };
    let mut out = String::with_capacity(SAFETY_NUMBER_DIGITS);
    for identity in [first, second] {
        push_party_digits(&mut out, identity);
    }
    out
}

fn push_party_digits(out: &mut String, identity: &[u8; 32]) {
    let mut hash = Sha512::new()
        .chain_update(b"cypher/v2/safety-number")
        .chain_update(identity)
        .finalize();
    for _ in 1..SAFETY_ITERATIONS {
        hash = Sha512::new()
            .chain_update(hash)
            .chain_update(identity)
            .finalize();
    }
    for group in hash
        .as_chunks::<GROUP_BYTES>()
        .0
        .iter()
        .take(DIGIT_GROUPS_PER_PARTY)
    {
        let value = group
            .iter()
            .fold(0u64, |acc, &byte| acc.wrapping_shl(8) | u64::from(byte));
        let digits = value.checked_rem(GROUP_MODULUS).unwrap_or_default();
        // Writing into a String cannot fail.
        let _ = write!(out, "{digits:05}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_fingerprints_differ_per_identity_and_are_stable() {
        assert_eq!(link_fingerprint(&[1; 32]), link_fingerprint(&[1; 32]));
        assert_ne!(link_fingerprint(&[1; 32]), link_fingerprint(&[2; 32]));
    }

    #[test]
    #[cfg_attr(miri, ignore = "10 400 SHA-512 rounds; minutes under Miri")]
    fn safety_numbers_are_symmetric_and_specific_to_the_pair() {
        let (a, b, c) = ([1; 32], [2; 32], [3; 32]);
        let ab = safety_number(&a, &b);
        assert_eq!(ab, safety_number(&b, &a), "both sides see the same number");
        assert_eq!(ab.len(), SAFETY_NUMBER_DIGITS);
        assert!(ab.bytes().all(|d| d.is_ascii_digit()));
        assert_ne!(ab, safety_number(&a, &c));
        // A party's half does not depend on the other party.
        assert_eq!(ab.get(..30), safety_number(&a, &c).get(..30));
    }
}
