use cypher_common::{Error, Result};

const PADDING_BUCKETS: [usize; 5] = [512, 1024, 2048, 4096, 8192];

/// Pad `plaintext` to the next fixed bucket size.
///
/// Format: `[u16 LE length][plaintext][zero padding to bucket]`.
/// Maximum plaintext size is 8190 bytes (8192 − 2-byte header).
pub fn pad(plaintext: &[u8]) -> Result<Vec<u8>> {
    let needed = plaintext.len() + 2; // 2-byte length prefix
    let bucket = PADDING_BUCKETS
        .iter()
        .find(|&&b| b >= needed)
        .ok_or_else(|| {
            Error::Protocol(format!(
                "plaintext too large for padding: {} bytes (max {})",
                plaintext.len(),
                PADDING_BUCKETS.last().unwrap() - 2
            ))
        })?;
    let mut buf = Vec::with_capacity(*bucket);
    buf.extend_from_slice(&(plaintext.len() as u16).to_le_bytes());
    buf.extend_from_slice(plaintext);
    buf.resize(*bucket, 0);
    Ok(buf)
}

/// Remove padding and return the original plaintext slice.
pub fn unpad(padded: &[u8]) -> Result<&[u8]> {
    if padded.len() < 2 {
        return Err(Error::Protocol("padded payload too short".into()));
    }
    let len = u16::from_le_bytes([padded[0], padded[1]]) as usize;
    if 2 + len > padded.len() {
        return Err(Error::Protocol(format!(
            "padding length prefix {len} exceeds payload size {}",
            padded.len() - 2
        )));
    }
    Ok(&padded[2..2 + len])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_small() {
        let data = b"hello";
        let padded = pad(data).unwrap();
        assert_eq!(padded.len(), 512);
        assert_eq!(unpad(&padded).unwrap(), data);
    }

    #[test]
    fn roundtrip_boundary() {
        // Exactly fills 512 bucket: 510 bytes data + 2 prefix = 512
        let data = vec![0xAB; 510];
        let padded = pad(&data).unwrap();
        assert_eq!(padded.len(), 512);
        assert_eq!(unpad(&padded).unwrap(), &data[..]);
    }

    #[test]
    fn roundtrip_exceeds_first_bucket() {
        // 511 bytes data + 2 prefix = 513 → next bucket 1024
        let data = vec![0xCD; 511];
        let padded = pad(&data).unwrap();
        assert_eq!(padded.len(), 1024);
        assert_eq!(unpad(&padded).unwrap(), &data[..]);
    }

    #[test]
    fn roundtrip_max_bucket() {
        let data = vec![0xFF; 8190];
        let padded = pad(&data).unwrap();
        assert_eq!(padded.len(), 8192);
        assert_eq!(unpad(&padded).unwrap(), &data[..]);
    }

    #[test]
    fn oversize_fails() {
        let data = vec![0x00; 8191];
        assert!(pad(&data).is_err());
    }

    #[test]
    fn unpad_too_short() {
        assert!(unpad(&[]).is_err());
        assert!(unpad(&[0x01]).is_err());
    }

    #[test]
    fn unpad_corrupt_length() {
        // length prefix says 1000 but buffer is only 512
        let mut buf = vec![0u8; 512];
        buf[0] = 0xE8; // 1000 as u16 LE
        buf[1] = 0x03;
        assert!(unpad(&buf).is_err());
    }

    #[test]
    fn all_buckets() {
        for &bucket in &PADDING_BUCKETS {
            let max_data = bucket - 2;
            let data = vec![0x42; max_data];
            let padded = pad(&data).unwrap();
            assert_eq!(padded.len(), bucket);
            assert_eq!(unpad(&padded).unwrap(), &data[..]);
        }
    }
}
