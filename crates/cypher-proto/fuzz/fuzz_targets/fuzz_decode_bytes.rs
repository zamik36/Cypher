#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = cypher_proto::decode_bytes(data, 0);
    let _ = cypher_proto::decode_string(data, 0);
});
