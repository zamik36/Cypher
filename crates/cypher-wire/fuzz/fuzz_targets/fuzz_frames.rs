#![no_main]
use bytes::Bytes;
use cypher_wire::{ClientMsg, Frame, ServerMsg, peek_send};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let b = Bytes::copy_from_slice(data);
    if let Ok(f) = Frame::<ClientMsg>::decode(b.clone()) {
        assert_eq!(Frame::<ClientMsg>::decode(f.encode()).ok(), Some(f));
    }
    if let Ok(f) = Frame::<ServerMsg>::decode(b.clone()) {
        assert_eq!(Frame::<ServerMsg>::decode(f.encode()).ok(), Some(f));
    }
    let _ = peek_send(&b);
});
