/// Failure modes of the v2 primitives. Deliberately coarse: callers must not
/// be able to distinguish *why* authentication failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CryptoError {
    #[error("authentication failed")]
    Aead,
    #[error("malformed input")]
    Malformed,
    #[error("invalid signature")]
    Signature,
    #[error("non-contributory key exchange")]
    NonContributory,
    #[error("too many skipped messages")]
    TooManySkipped,
    #[error("session cannot send before it has received")]
    NotReady,
}

impl From<CryptoError> for cypher_types::Error {
    fn from(e: CryptoError) -> Self {
        Self::Crypto(e.to_string())
    }
}
