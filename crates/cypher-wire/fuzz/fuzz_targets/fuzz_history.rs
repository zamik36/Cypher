//! cypher_core::history::Reader, the archive a linked device receives. The
//! input is cut into pieces at points the fuzzer picks; read piece by piece
//! it must give exactly what it gives read whole, and never panic. With the
//! top bit of the first byte set, the rest drives a Writer instead and the
//! archive it writes must read back to what was written.
#![no_main]
use cypher_core::Table;
use cypher_core::history::{ArchiveError, Item, Reader, Writer};
use cypher_types::FileId;
use libfuzzer_sys::fuzz_target;

#[derive(Debug, PartialEq, Eq)]
enum Ev {
    Row(Table, Vec<u8>, Vec<u8>),
    Blob(FileId, u64),
    Data(Vec<u8>),
    End,
    Failed(ArchiveError),
}

/// Drains what the reader can give now; false once it failed.
fn drain(reader: &mut Reader, out: &mut Vec<Ev>) -> bool {
    loop {
        let ev = match reader.next_item() {
            Ok(None) => return true,
            Ok(Some(Item::Row { table, key, value })) => {
                Ev::Row(table, key.to_vec(), value.to_vec())
            }
            Ok(Some(Item::BlobStart { file_id, len })) => Ev::Blob(file_id, len),
            Ok(Some(Item::BlobData(bytes))) => {
                assert!(!bytes.is_empty(), "empty blob piece");
                if let Some(Ev::Data(prev)) = out.last_mut() {
                    prev.extend_from_slice(bytes);
                    continue;
                }
                Ev::Data(bytes.to_vec())
            }
            Ok(Some(Item::End)) => Ev::End,
            Err(e) => {
                out.push(Ev::Failed(e));
                return false;
            }
        };
        out.push(ev);
    }
}

fn read(pieces: &[&[u8]]) -> (Vec<Ev>, Result<(), ArchiveError>) {
    let mut reader = Reader::new();
    let mut out = Vec::new();
    for piece in pieces {
        reader.push(piece);
        if !drain(&mut reader, &mut out) {
            return (out, Err(ArchiveError::Malformed));
        }
    }
    let finished = reader.finish();
    (out, finished)
}

fn split_and_compare(data: &[u8]) {
    let Some((&n, rest)) = data.split_first() else {
        return;
    };
    let n = usize::from(n % 16);
    let (cuts, archive) = rest.split_at(rest.len().min(2 * n));
    let mut at: Vec<usize> = cuts
        .chunks(2)
        .map(|c| {
            usize::from(u16::from_le_bytes([c[0], *c.get(1).unwrap_or(&0)])) % (archive.len() + 1)
        })
        .collect();
    at.sort_unstable();
    let mut pieces = Vec::with_capacity(at.len() + 1);
    let mut prev = 0;
    for cut in at {
        pieces.push(&archive[prev..cut]);
        prev = cut;
    }
    pieces.push(&archive[prev..]);
    let whole = read(&[archive]);
    let streamed = read(&pieces);
    assert_eq!(whole, streamed, "pieces read differently from the whole");
}

fn write_and_read(spec: &[u8]) {
    let mut out = Vec::new();
    let mut writer = Writer::default();
    let mut expected = Vec::new();
    Writer::header(&mut out);
    for (i, chunk) in spec.chunks(24).enumerate() {
        let kind = chunk.first().copied().unwrap_or(0) % 3;
        if kind == 2 {
            let id = FileId(std::array::from_fn(|k| chunk.get(k).copied().unwrap_or(0)));
            let blob = chunk.get(1..).unwrap_or_default();
            writer
                .row(&mut out, Table::Media, id.as_bytes(), chunk)
                .unwrap();
            writer.blob(&mut out, &id, blob.len() as u64);
            out.extend_from_slice(blob);
            expected.push(Ev::Row(
                Table::Media,
                id.as_bytes().to_vec(),
                chunk.to_vec(),
            ));
            expected.push(Ev::Blob(id, blob.len() as u64));
            if !blob.is_empty() {
                expected.push(Ev::Data(blob.to_vec()));
            }
        } else {
            let table = if kind == 0 {
                Table::Messages
            } else {
                Table::MessageStatus
            };
            let key = &chunk[..chunk.len().min(1 + i % 8)];
            writer.row(&mut out, table, key, chunk).unwrap();
            expected.push(Ev::Row(table, key.to_vec(), chunk.to_vec()));
        }
    }
    writer.end(&mut out);
    expected.push(Ev::End);
    let (got, finished) = read(&[&out]);
    assert_eq!(got, expected, "archive did not read back");
    assert_eq!(finished, Ok(()));
    let (got, finished) = read(&out.chunks(7).collect::<Vec<_>>());
    assert_eq!(got, expected, "archive in pieces did not read back");
    assert_eq!(finished, Ok(()));
}

fuzz_target!(|data: &[u8]| {
    match data.split_first() {
        Some((&mode, rest)) if mode & 0x80 != 0 => write_and_read(rest),
        _ => split_and_compare(data),
    }
});
