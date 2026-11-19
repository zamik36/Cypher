#![no_main]

use cypher_wire::relay::{self, Route};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(request) = relay::parse(data) {
        match request.route {
            Route::Onion => {}
            Route::Keys => assert!(request.payload.is_empty()),
        }
    }
    let _ = relay::parse_reply(data);
});
