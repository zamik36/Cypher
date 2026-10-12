//! The message history archive in the web worker: written while it reads
//! the stores, and read back with every row checked against the profile's
//! vault before it is stored.

use cypher_core::Table;
use cypher_core::Vault;
use cypher_core::history::{self, Item, Reader, Writer};
use serde::Serialize;
use wasm_bindgen::prelude::*;

use crate::command::{file, file_size};
use crate::convert::{js_number, to_js};
use crate::js_err;

#[wasm_bindgen(typescript_custom_section)]
const TS_TYPES: &str = r#"
/** A table the history archive carries, as its `IndexedDB` store is named. */
export type HistoryTable = "messages" | "message_status" | "media";

/** What the history reader found next. */
export type HistoryItem =
  | { kind: "row"; table: HistoryTable; key: Uint8Array<ArrayBuffer>; value: Uint8Array<ArrayBuffer> }
  | { kind: "blob"; file_id: string; len: number }
  | { kind: "data"; data: Uint8Array<ArrayBuffer> }
  | { kind: "end" };
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

/// Reads an archive pushed to it in pieces, yielding only rows this profile
/// sealed.
#[wasm_bindgen]
pub struct HistoryReader {
    reader: Reader,
    vault: Vault,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum JsItem<'a> {
    Row {
        table: &'static str,
        #[serde(with = "serde_bytes")]
        key: &'a [u8],
        #[serde(with = "serde_bytes")]
        value: &'a [u8],
    },
    Blob {
        file_id: String,
        len: f64,
    },
    Data {
        #[serde(with = "serde_bytes")]
        data: &'a [u8],
    },
    End,
}

impl HistoryReader {
    pub(crate) fn new(vault: Vault) -> Self {
        Self {
            reader: Reader::new(),
            vault,
        }
    }
}

#[wasm_bindgen]
impl HistoryReader {
    pub fn push(&mut self, piece: &[u8]) {
        self.reader.push(piece);
    }

    /// The next item, or `undefined` until more is pushed. Rows this
    /// profile did not seal under their key are passed over.
    #[wasm_bindgen(js_name = next, unchecked_return_type = "HistoryItem | undefined")]
    pub fn next_item(&mut self) -> Result<JsValue, JsError> {
        loop {
            let Some(item) = self.reader.next_item().map_err(js_err)? else {
                return Ok(JsValue::UNDEFINED);
            };
            let item = match item {
                Item::Row { table, key, value } => {
                    if !history::check_row(&self.vault, table, key, value) {
                        continue;
                    }
                    JsItem::Row {
                        table: table.name(),
                        key,
                        value,
                    }
                }
                Item::BlobStart { file_id, len } => JsItem::Blob {
                    file_id: file_id.to_hex(),
                    len: js_number(len),
                },
                Item::BlobData(data) => JsItem::Data { data },
                Item::End => JsItem::End,
            };
            return to_js(&item);
        }
    }

    /// The archive is over: it must have been whole.
    pub fn finish(&self) -> Result<(), JsError> {
        self.reader.finish().map_err(js_err)
    }
}
