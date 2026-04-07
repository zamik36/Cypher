use cypher_common::{Error, Result};
use cypher_proto::{dispatch, Message};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use x25519_dalek::PublicKey as X25519PublicKey;

pub const CAPABILITY_SIGNED_INBOX: u32 = 1 << 0;
pub const CAPABILITY_RELAY: u32 = 1 << 1;

#[derive(Debug, Clone)]
pub struct RelayBootstrap {
    pub addr: String,
    pub public_key: X25519PublicKey,
}

#[derive(Debug, Clone)]
pub struct TransportBootstrap {
    pub relay: Option<RelayBootstrap>,
    pub inbox_verifying_key: VerifyingKey,
    pub capabilities: u32,
}

impl TransportBootstrap {
    pub fn from_proto(info: cypher_proto::TransportBootstrapInfo) -> Result<Self> {
        let inbox_verifying_key = VerifyingKey::from_bytes(
            &info
                .inbox_verifying_key
                .as_slice()
                .try_into()
                .map_err(|_| Error::Protocol("invalid inbox verifying key length".into()))?,
        )
        .map_err(|e| Error::Protocol(format!("invalid inbox verifying key: {e}")))?;

        let relay = if info.capabilities & CAPABILITY_RELAY != 0 {
            if info.relay_addr.trim().is_empty() {
                return Err(Error::Protocol(
                    "transport bootstrap missing relay_addr".into(),
                ));
            }
            let relay_public_key: [u8; 32] = info
                .relay_public_key
                .as_slice()
                .try_into()
                .map_err(|_| Error::Protocol("invalid relay public key length".into()))?;
            Some(RelayBootstrap {
                addr: info.relay_addr,
                public_key: X25519PublicKey::from(relay_public_key),
            })
        } else {
            None
        };

        Ok(Self {
            relay,
            inbox_verifying_key,
            capabilities: info.capabilities,
        })
    }

    pub fn supports_signed_inbox(&self) -> bool {
        self.capabilities & CAPABILITY_SIGNED_INBOX != 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedInboxResponse {
    pub proto_payload: Vec<u8>,
    pub claim_token: Option<Vec<u8>>,
    pub server_sig: Option<[u8; 64]>,
}

impl SignedInboxResponse {
    const CLAIM_TOKEN_LEN: usize = 40;
    const SERVER_SIG_LEN: usize = 64;
    const TRAILER_LEN: usize = Self::CLAIM_TOKEN_LEN + Self::SERVER_SIG_LEN;

    pub fn parse(raw: &[u8]) -> Result<Self> {
        if raw.len() > Self::TRAILER_LEN {
            let proto_len = raw.len() - Self::TRAILER_LEN;
            let proto_payload = raw[..proto_len].to_vec();
            if matches!(dispatch(&proto_payload), Ok(Message::InboxMessages(_))) {
                let mut server_sig = [0u8; Self::SERVER_SIG_LEN];
                server_sig.copy_from_slice(&raw[proto_len + Self::CLAIM_TOKEN_LEN..]);
                return Ok(Self {
                    proto_payload,
                    claim_token: Some(raw[proto_len..proto_len + Self::CLAIM_TOKEN_LEN].to_vec()),
                    server_sig: Some(server_sig),
                });
            }
        }

        if matches!(dispatch(raw), Ok(Message::InboxMessages(_))) {
            return Ok(Self {
                proto_payload: raw.to_vec(),
                claim_token: None,
                server_sig: None,
            });
        }

        Err(Error::Protocol("invalid inbox response payload".into()))
    }

    pub fn verify(&self, verifying_key: &VerifyingKey, inbox_id: &[u8]) -> Result<()> {
        let Some(claim_token) = self.claim_token.as_ref() else {
            return Ok(());
        };
        let Some(server_sig) = self.server_sig.as_ref() else {
            return Err(Error::Protocol("missing inbox response signature".into()));
        };

        let timestamp = claim_token_timestamp(claim_token)?;
        let inbox = match dispatch(&self.proto_payload)? {
            Message::InboxMessages(inbox) => inbox,
            _ => {
                return Err(Error::Protocol(
                    "signed inbox response did not contain InboxMessages".into(),
                ))
            }
        };

        let mut signed = Vec::with_capacity(inbox.messages.len() + 4 + inbox_id.len() + 8);
        signed.extend_from_slice(&inbox.messages);
        signed.extend_from_slice(&inbox.count.to_le_bytes());
        signed.extend_from_slice(inbox_id);
        signed.extend_from_slice(&timestamp.to_le_bytes());

        let signature = Signature::from_bytes(server_sig);
        verifying_key
            .verify(&signed, &signature)
            .map_err(|e| Error::Protocol(format!("invalid inbox response signature: {e}")))
    }
}

pub fn claim_token_timestamp(token: &[u8]) -> Result<u64> {
    if token.len() != SignedInboxResponse::CLAIM_TOKEN_LEN {
        return Err(Error::Protocol("invalid claim token length".into()));
    }
    let ts: [u8; 8] = token[..8]
        .try_into()
        .map_err(|_| Error::Protocol("invalid claim token timestamp".into()))?;
    Ok(u64::from_le_bytes(ts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::onion::circuit::Circuit;
    use crate::onion::encoder;
    use cypher_proto::Serializable;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn parses_legacy_inbox_payload() {
        let payload = cypher_proto::InboxMessages {
            messages: b"hello".to_vec(),
            count: 1,
        }
        .serialize();

        let parsed = SignedInboxResponse::parse(&payload).unwrap();
        assert_eq!(parsed.proto_payload, payload);
        assert!(parsed.claim_token.is_none());
        assert!(parsed.server_sig.is_none());
    }

    #[test]
    fn rejects_tampered_signature() {
        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        let verifying_key = signing_key.verifying_key();
        let inbox_id = b"inbox";
        let timestamp = 1234u64;
        let inbox = cypher_proto::InboxMessages {
            messages: b"ciphertext".to_vec(),
            count: 1,
        };
        let proto_payload = inbox.serialize();
        let mut signed = Vec::new();
        signed.extend_from_slice(&inbox.messages);
        signed.extend_from_slice(&inbox.count.to_le_bytes());
        signed.extend_from_slice(inbox_id);
        signed.extend_from_slice(&timestamp.to_le_bytes());

        let mut raw = proto_payload.clone();
        let mut claim_token = Vec::from(timestamp.to_le_bytes());
        claim_token.extend_from_slice(&[9u8; 32]);
        raw.extend_from_slice(&claim_token);
        let mut signature = signing_key.sign(&signed).to_bytes();
        signature[0] ^= 0xFF;
        raw.extend_from_slice(&signature);

        let parsed = SignedInboxResponse::parse(&raw).unwrap();
        assert!(parsed.verify(&verifying_key, inbox_id).is_err());
    }

    #[test]
    fn claim_token_timestamp_roundtrip() {
        let ts = 999u64;
        let mut token = Vec::from(ts.to_le_bytes());
        token.extend_from_slice(&[0u8; 32]);
        assert_eq!(claim_token_timestamp(&token).unwrap(), ts);
    }

    #[test]
    fn unrelated_onion_payload_is_not_inbox() {
        let circuit = Circuit::new(&X25519PublicKey::from([1u8; 32]));
        let raw = encoder::encode_relay_request(&circuit, b"hello").unwrap();
        assert!(SignedInboxResponse::parse(&raw).is_err());
    }

    #[test]
    fn bootstrap_requires_relay_fields_when_capability_is_set() {
        let info = cypher_proto::TransportBootstrapInfo {
            relay_addr: String::new(),
            relay_public_key: Vec::new(),
            inbox_verifying_key: SigningKey::from_bytes(&[1u8; 32])
                .verifying_key()
                .to_bytes()
                .to_vec(),
            capabilities: CAPABILITY_SIGNED_INBOX | CAPABILITY_RELAY,
        };

        assert!(TransportBootstrap::from_proto(info).is_err());
    }
}
