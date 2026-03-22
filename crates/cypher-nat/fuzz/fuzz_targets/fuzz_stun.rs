#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let txn_id = [0u8; 12];
    let _ = cypher_nat::parse_binding_response(data, &txn_id);
});
