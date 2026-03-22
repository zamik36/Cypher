use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error("Crypto error: {0}")]
    Crypto(String),

    #[error("Transport error: {0}")]
    Transport(String),

    #[error("Session error: {0}")]
    Session(String),

    #[error("NAT traversal error: {0}")]
    Nat(String),

    #[error("Transfer error: {0}")]
    Transfer(String),

    #[error("Config error: {0}")]
    Config(String),

    #[error("Connection closed")]
    ConnectionClosed,

    #[error("Timeout")]
    Timeout,

    #[error("Peer not found: {0}")]
    PeerNotFound(String),

    #[error("Invalid data: {0}")]
    InvalidData(String),
}
