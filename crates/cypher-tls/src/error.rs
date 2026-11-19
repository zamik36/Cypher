use cypher_types::Error;
use rustls::pki_types::pem;

pub(crate) fn transport(context: &str, e: impl std::fmt::Display) -> Error {
    Error::Transport(format!("{context}: {e}"))
}

/// A PEM parse error as text: the markers and lines it quotes are bytes,
/// shown as a byte list by its own `Display`.
pub(crate) fn pem_reason(e: &pem::Error) -> String {
    match e {
        pem::Error::MissingSectionEnd { end_marker } => {
            // The marker is the section label, e.g. `CERTIFICATE`.
            format!(
                "missing -----END {}-----",
                String::from_utf8_lossy(end_marker)
            )
        }
        pem::Error::IllegalSectionStart { line } => {
            format!("illegal section start {}", String::from_utf8_lossy(line))
        }
        other => other.to_string(),
    }
}
