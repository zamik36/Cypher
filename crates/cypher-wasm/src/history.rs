//! The message history archive in the web worker: written while it reads
//! the stores, and read back with every row checked against the profile's
//! vault before it is stored.

use cypher_core::Table;
use cypher_core::Vault;
use cypher_core::history::{Import, Reader, Step, Writer};
use serde::Serialize;
use wasm_bindgen::prelude::*;

use crate::command::{file, file_size};
use crate::convert::to_js;
use crate::js_err;

#[wasm_bindgen(typescript_custom_section)]
const TS_TYPES: &str = r#"
/** A table the history archive carries, as its `IndexedDB` store is named. */
export type HistoryTable = "messages" | "message_status" | "media";

/**
 * What to do next while taking in an archive: store a row unless its key is
 * taken; write a note (`note_start`, `note_data`) where notes are kept unless
 * its media row is stored already; once it is whole (`note_end`), store that
 * row unless its key is taken.
 */
export type HistoryStep =
  | { kind: "row"; table: HistoryTable; key: Uint8Array<ArrayBuffer>; value: Uint8Array<ArrayBuffer> }
  | { kind: "note_start"; file_id: string; key: Uint8Array<ArrayBuffer> }
  | { kind: "note_data"; data: Uint8Array<ArrayBuffer> }
  | { kind: "note_end"; key: Uint8Array<ArrayBuffer>; value: Uint8Array<ArrayBuffer> };
"#;

fn carried(name: &str) -> Option<Table> {
    [Table::Messages, Table::MessageStatus, Table::Media]
        .into_iter()
        .find(|t| t.name() == name)
}

/// Writes an archive in pieces the worker appends to its file.
#[wasm_bindgen]
#[derive(Default)]
pub struct HistoryWriter {
    inner: Writer,
}

#[wasm_bindgen]
impl HistoryWriter {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn header() -> Vec<u8> {
        let mut out = Vec::new();
        Writer::header(&mut out);
        out
    }

    /// The record of a row, or nothing when the archive does not carry it
    /// (another table, or too big).
    pub fn row(&mut self, table: &str, key: &[u8], value: &[u8]) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        let table = carried(table)?;
        self.inner.row(&mut out, table, key, value).ok()?;
        Some(out)
    }

    /// Starts the note of the media row just written; `len` bytes follow.
    pub fn blob(&mut self, file_id: &str, len: f64) -> Result<Vec<u8>, JsError> {
        let mut out = Vec::new();
        let len = file_size(len).map_err(js_err)?;
        self.inner
            .blob(&mut out, &file(file_id).map_err(js_err)?, len);
        Ok(out)
    }

    pub fn end(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.inner.end(&mut out);
        out
    }
}

/// Reads an archive pushed to it in pieces and says what to do with it
/// ([`Import`]): only rows this profile sealed, only whole notes.
#[wasm_bindgen]
pub struct HistoryReader {
    reader: Reader,
    import: Import,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum JsStep<'a> {
    Row {
        table: &'static str,
        #[serde(with = "serde_bytes")]
        key: &'a [u8],
        #[serde(with = "serde_bytes")]
        value: &'a [u8],
    },
    NoteStart {
        file_id: String,
        #[serde(with = "serde_bytes")]
        key: &'a [u8],
    },
    NoteData {
        #[serde(with = "serde_bytes")]
        data: &'a [u8],
    },
    NoteEnd {
        #[serde(with = "serde_bytes")]
        key: &'a [u8],
        #[serde(with = "serde_bytes")]
        value: &'a [u8],
    },
}

fn step_to_js(step: &Step<'_>) -> Result<JsValue, JsError> {
    to_js(&match step {
        Step::Row { table, key, value } => JsStep::Row {
            table: table.name(),
            key,
            value,
        },
        Step::NoteStart { file_id } => JsStep::NoteStart {
            file_id: file_id.to_hex(),
            key: file_id.as_bytes(),
        },
        Step::NoteData(data) => JsStep::NoteData { data },
        Step::NoteEnd { key, value, .. } => JsStep::NoteEnd { key, value },
    })
}

impl HistoryReader {
    pub(crate) fn new(vault: Vault) -> Self {
        Self {
            reader: Reader::new(),
            import: Import::new(vault),
        }
    }
}

#[wasm_bindgen]
impl HistoryReader {
    pub fn push(&mut self, piece: &[u8]) {
        self.reader.push(piece);
    }

    /// The steps of the next item, in order (possibly none), or
    /// `undefined` until more is pushed.
    #[wasm_bindgen(js_name = next, unchecked_return_type = "HistoryStep[] | undefined")]
    pub fn next_steps(&mut self) -> Result<JsValue, JsError> {
        let Some(item) = self.reader.next_item().map_err(js_err)? else {
            return Ok(JsValue::UNDEFINED);
        };
        let steps = js_sys::Array::new();
        for step in self.import.take(item) {
            steps.push(&step_to_js(&step)?);
        }
        Ok(steps.into())
    }

    /// The archive is over: it must have been whole.
    pub fn finish(&self) -> Result<(), JsError> {
        self.reader.finish().map_err(js_err)
    }
}
