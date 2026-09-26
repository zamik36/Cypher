#![no_main]
use bytes::Bytes;
use cypher_core::envelope::Envelope;
use cypher_core::relay::RelayBody;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = RelayBody::decode(Bytes::copy_from_slice(data));
    let _ = Envelope::decode(data);
    let _ = cypher_core::fs_name::sanitize(&String::from_utf8_lossy(data));
});
