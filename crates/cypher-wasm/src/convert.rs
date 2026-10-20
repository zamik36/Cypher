use cypher_core::{Effect, Event, Rows, Sink, Snapshot, StoreOp, Table, ui};
use js_sys::{Array, Uint8Array};
use serde::Serialize;
use wasm_bindgen::prelude::*;

/// TypeScript shapes of the values below, emitted into the generated `.d.ts`
/// so the worker is typed from the same place the values are serialized.
#[wasm_bindgen(typescript_custom_section)]
const TS_TYPES: &str = r#"
/** One `IndexedDB` row: key and value bytes. */
export type Row = [Uint8Array<ArrayBuffer>, Uint8Array<ArrayBuffer>];

/** The rows the client restores from, by `IndexedDB` store. */
export interface StoredState {
  meta: Row[];
  peers: Row[];
  sessions: Row[];
  outbox: Row[];
  transfers: Row[];
}

/** A storage operation; a batch of them is applied atomically. */
export type Op =
  | { kind: "put"; table: string; key: Uint8Array<ArrayBuffer>; value: Uint8Array<ArrayBuffer> }
  | { kind: "delete"; table: string; key: Uint8Array<ArrayBuffer> };

/** Outcome of a pending `create_link` / `join_link` call. */
export interface Reply { kind: "reply"; op: string; value: string; link?: string | undefined }

export interface ReadChunk { kind: "read_chunk"; file_id: string; index: number; offset: number; len: number; headroom: number }

export type Effect =
  | { kind: "transmit" | "anonymous"; data: Uint8Array<ArrayBuffer> }
  | Op
  | { kind: "event"; channel: string; payload: unknown }
  | Reply
  | ReadChunk
  | { kind: "open_sink"; file_id: string; len: number; sink: "download" | "media" | "history" }
  | { kind: "write_chunk"; file_id: string; offset: number; data: Uint8Array<ArrayBuffer> }
  | { kind: "close_sink"; file_id: string; complete: boolean }
  | { kind: "build_history"; file_id: string; partial: string[] }
  | { kind: "import_history"; file_id: string }
  | { kind: "disconnect"; reconnect: boolean };

export interface CommandOutcome { effects: Effect[]; msgId?: string; fileId?: string }

export interface OpenedMedia { mime: string; bytes: Uint8Array<ArrayBuffer> }
"#;

// Each value is built and serialized immediately, one at a time; boxing the
// large `Event` payload would only add an allocation per effect.
#[cfg_attr(
    target_pointer_width = "64",
    expect(clippy::large_enum_variant, reason = "short-lived, serialized at once")
)]
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum JsEffect<'a> {
    Transmit {
        #[serde(with = "serde_bytes")]
        data: &'a [u8],
    },
    Anonymous {
        #[serde(with = "serde_bytes")]
        data: &'a [u8],
    },
    Put {
        table: &'static str,
        #[serde(with = "serde_bytes")]
        key: &'a [u8],
        #[serde(with = "serde_bytes")]
        value: &'a [u8],
    },
    Delete {
        table: &'static str,
        #[serde(with = "serde_bytes")]
        key: &'a [u8],
    },
    Event {
        channel: &'static str,
        payload: ui::UiPayload,
    },
    ReadChunk {
        file_id: String,
        index: u32,
        offset: f64,
        len: u32,
        headroom: u32,
    },
    OpenSink {
        file_id: String,
        len: f64,
        sink: &'static str,
    },
    WriteChunk {
        file_id: String,
        offset: f64,
        #[serde(with = "serde_bytes")]
        data: &'a [u8],
    },
    CloseSink {
        file_id: String,
        complete: bool,
    },
    BuildHistory {
        file_id: String,
        partial: Vec<String>,
    },
    ImportHistory {
        file_id: String,
    },
    Disconnect {
        reconnect: bool,
    },
    /// Outcome of a pending `create_link` / `join_link` call.
    Reply {
        op: &'static str,
        value: String,
        link: Option<&'a str>,
    },
}

fn reply(event: &Event) -> Option<JsEffect<'_>> {
    let (op, value, link) = match event {
        Event::LinkCreated { link } => ("link_created", link.clone(), None),
        Event::PeerAdded {
            peer,
            initiated_by_us: true,
        } => ("joined", peer.to_hex(), None),
        Event::JoinFailed { link, reason } => {
            ("join_failed", format!("{reason:?}"), Some(link.as_str()))
        }
        Event::Warning { reason } => ("warning", format!("{reason:?}"), None),
        _ => return None,
    };
    Some(JsEffect::Reply { op, value, link })
}

pub(crate) fn to_js<T: Serialize>(value: &T) -> Result<JsValue, JsError> {
    value
        .serialize(&serde_wasm_bindgen::Serializer::new().serialize_maps_as_objects(true))
        .map_err(|e| JsError::new(&e.to_string()))
}

pub(crate) fn effects_to_js(effects: &[Effect]) -> Result<Array, JsError> {
    let out = Array::new();
    for effect in effects {
        if let Effect::Emit(event) = effect
            && let Some(r) = reply(event)
        {
            out.push(&to_js(&r)?);
        }
        if let Some(js) = js_effect(effect) {
            out.push(&to_js(&js)?);
        }
    }
    Ok(out)
}

/// The JS shape of one effect; `None` for events the UI never sees.
fn js_effect(effect: &Effect) -> Option<JsEffect<'_>> {
    Some(match effect {
        Effect::Transmit(b) => JsEffect::Transmit { data: b },
        Effect::Anonymous(b) => JsEffect::Anonymous { data: b },
        Effect::Persist(StoreOp::Put { table, key, value }) => JsEffect::Put {
            table: table.name(),
            key,
            value,
        },
        Effect::Persist(StoreOp::Delete { table, key }) => JsEffect::Delete {
            table: table.name(),
            key,
        },
        Effect::Emit(event) => {
            let (channel, payload) = ui::event(event)?;
            JsEffect::Event { channel, payload }
        }
        Effect::ReadChunk {
            file_id,
            index,
            offset,
            len,
            headroom,
        } => JsEffect::ReadChunk {
            file_id: file_id.to_hex(),
            index: *index,
            offset: js_number(*offset),
            len: *len,
            headroom: u32::try_from(*headroom).unwrap_or(u32::MAX),
        },
        Effect::OpenSink { file_id, len, sink } => JsEffect::OpenSink {
            file_id: file_id.to_hex(),
            len: js_number(*len),
            sink: sink_name(*sink),
        },
        Effect::WriteChunk {
            file_id,
            offset,
            data,
        } => JsEffect::WriteChunk {
            file_id: file_id.to_hex(),
            offset: js_number(*offset),
            data,
        },
        Effect::CloseSink { file_id, complete } => JsEffect::CloseSink {
            file_id: file_id.to_hex(),
            complete: *complete,
        },
        Effect::BuildHistory { file_id, partial } => JsEffect::BuildHistory {
            file_id: file_id.to_hex(),
            partial: partial.iter().map(|id| id.to_hex()).collect(),
        },
        Effect::ImportHistory { file_id } => JsEffect::ImportHistory {
            file_id: file_id.to_hex(),
        },
        Effect::Disconnect { reconnect } => JsEffect::Disconnect {
            reconnect: *reconnect,
        },
    })
}

fn sink_name(sink: Sink) -> &'static str {
    match sink {
        Sink::Download => "download",
        Sink::Media => "media",
        Sink::History => "history",
    }
}

/// File offsets and sizes as JS numbers.
#[expect(
    clippy::cast_precision_loss,
    reason = "files are capped at 64 GiB, far inside the 2^53 exact range of JS numbers"
)]
pub(crate) fn js_number(v: u64) -> f64 {
    v as f64
}

/// `[[Uint8Array, Uint8Array], ...]` → owned key/value pairs.
/// The snapshot a `StoredState` holds.
pub(crate) fn snapshot_from_js(state: &JsValue) -> Result<Snapshot, JsError> {
    let table = |t: Table| -> Result<Rows, JsError> {
        let rows = js_sys::Reflect::get(state, &JsValue::from_str(t.name()))
            .map_err(|_| JsError::new("stored state must be an object"))?
            .dyn_into::<Array>()
            .map_err(|_| JsError::new("each store must be an array of rows"))?;
        pairs_from_js(&rows)
    };
    Ok(Snapshot {
        meta: table(Table::Meta)?,
        peers: table(Table::Peers)?,
        sessions: table(Table::Sessions)?,
        outbox: table(Table::Outbox)?,
        transfers: table(Table::Transfers)?,
    })
}

pub(crate) fn pairs_from_js(rows: &Array) -> Result<Rows, JsError> {
    rows.iter()
        .map(|row| {
            let pair: Array = row
                .dyn_into()
                .map_err(|_| JsError::new("row must be [key, value]"))?;
            let bytes = |v: JsValue| -> Result<Vec<u8>, JsError> {
                Ok(v.dyn_into::<Uint8Array>()
                    .map_err(|_| JsError::new("expected Uint8Array"))?
                    .to_vec())
            };
            Ok((bytes(pair.get(0))?, bytes(pair.get(1))?))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use cypher_core::FailReason;
    use cypher_types::{FileId, PeerId};
    use serde_json::{Value, json};

    use super::*;

    const FILE: FileId = FileId([0xAB; 16]);

    /// The serde shape of an effect: its tags, field names and number
    /// types. Rendered through `serde_json`, so bytes show as arrays where
    /// the worker gets a `Uint8Array` from `serde_wasm_bindgen`.
    fn shape(effect: &Effect) -> Option<Value> {
        js_effect(effect).map(|js| serde_json::to_value(js).unwrap())
    }

    fn file_hex() -> String {
        "ab".repeat(16)
    }

    /// The tags, field names and number types the web worker reads
    /// (`Effect` in the generated `.d.ts`), one per effect, so a renamed
    /// tag or field fails here first.
    #[test]
    fn effects_keep_the_shape_the_worker_reads() {
        let golden = [
            (
                Effect::Transmit(Bytes::from_static(&[1, 2])),
                json!({ "kind": "transmit", "data": [1, 2] }),
            ),
            (
                Effect::Anonymous(Bytes::from_static(&[3])),
                json!({ "kind": "anonymous", "data": [3] }),
            ),
            (
                Effect::Persist(StoreOp::Put {
                    table: Table::Outbox,
                    key: vec![4],
                    value: vec![5, 6],
                }),
                json!({ "kind": "put", "table": "outbox", "key": [4], "value": [5, 6] }),
            ),
            (
                Effect::Persist(StoreOp::Delete {
                    table: Table::Sessions,
                    key: vec![7],
                }),
                json!({ "kind": "delete", "table": "sessions", "key": [7] }),
            ),
            (
                Effect::Emit(Event::Connected),
                json!({ "kind": "event", "channel": "connected", "payload": null }),
            ),
            (
                Effect::Emit(Event::TransferComplete { file_id: FILE }),
                json!({ "kind": "event", "channel": "file_complete", "payload": file_hex() }),
            ),
            (
                Effect::Disconnect { reconnect: false },
                json!({ "kind": "disconnect", "reconnect": false }),
            ),
        ];
        for (effect, expected) in golden {
            assert_eq!(shape(&effect), Some(expected), "{effect:?}");
        }
    }

    /// The file effects, with ids as hex and offsets as JS numbers.
    #[test]
    fn file_effects_keep_the_shape_the_worker_reads() {
        let golden = [
            (
                Effect::ReadChunk {
                    file_id: FILE,
                    index: 2,
                    offset: 1 << 40,
                    len: 4096,
                    headroom: 16,
                },
                json!({
                    "kind": "read_chunk", "file_id": file_hex(), "index": 2,
                    "offset": 1_099_511_627_776.0, "len": 4096, "headroom": 16
                }),
            ),
            (
                Effect::WriteChunk {
                    file_id: FILE,
                    offset: 8,
                    data: Bytes::from_static(&[9]),
                },
                json!({ "kind": "write_chunk", "file_id": file_hex(), "offset": 8.0, "data": [9] }),
            ),
            (
                Effect::CloseSink {
                    file_id: FILE,
                    complete: true,
                },
                json!({ "kind": "close_sink", "file_id": file_hex(), "complete": true }),
            ),
            (
                Effect::BuildHistory {
                    file_id: FILE,
                    partial: vec![FileId([1; 16])],
                },
                json!({
                    "kind": "build_history", "file_id": file_hex(),
                    "partial": ["01".repeat(16)]
                }),
            ),
            (
                Effect::ImportHistory { file_id: FILE },
                json!({ "kind": "import_history", "file_id": file_hex() }),
            ),
        ];
        for (effect, expected) in golden {
            assert_eq!(shape(&effect), Some(expected), "{effect:?}");
        }
    }

    #[test]
    fn sinks_are_named_for_the_worker() {
        for (sink, name) in [
            (Sink::Download, "download"),
            (Sink::Media, "media"),
            (Sink::History, "history"),
        ] {
            let open = Effect::OpenSink {
                file_id: FILE,
                len: 10,
                sink,
            };
            let expected = json!({
                "kind": "open_sink", "file_id": file_hex(), "len": 10.0, "sink": name
            });
            assert_eq!(shape(&open), Some(expected));
        }
    }

    /// A read's headroom beyond what a JS `u32` holds is clamped, not
    /// wrapped.
    #[cfg(target_pointer_width = "64")]
    #[test]
    fn an_oversized_headroom_saturates() {
        let read = Effect::ReadChunk {
            file_id: FILE,
            index: 0,
            offset: 0,
            len: 1,
            headroom: usize::MAX,
        };
        let headroom = shape(&read).and_then(|v| v.get("headroom").cloned());
        assert_eq!(headroom, Some(json!(u32::MAX)));
    }

    /// The outcomes the worker's pending link and join calls wait for come
    /// as replies; other events have none.
    #[test]
    fn link_and_join_outcomes_become_replies() {
        let link = "cypher://join/abc".to_owned();
        let created = Event::LinkCreated { link: link.clone() };
        assert_eq!(shape(&Effect::Emit(created.clone())), None);
        let peer = PeerId([2; 32]);
        let golden = [
            (
                created,
                json!({ "kind": "reply", "op": "link_created", "value": link, "link": null }),
            ),
            (
                Event::PeerAdded {
                    peer,
                    initiated_by_us: true,
                },
                json!({ "kind": "reply", "op": "joined", "value": peer.to_hex(), "link": null }),
            ),
            (
                Event::JoinFailed {
                    link: link.clone(),
                    reason: FailReason::NotFound,
                },
                json!({ "kind": "reply", "op": "join_failed", "value": "NotFound", "link": link }),
            ),
            (
                Event::Warning {
                    reason: FailReason::Timeout,
                },
                json!({ "kind": "reply", "op": "warning", "value": "Timeout", "link": null }),
            ),
        ];
        for (event, expected) in golden {
            let js = reply(&event).map(|r| serde_json::to_value(r).unwrap());
            assert_eq!(js, Some(expected), "{event:?}");
        }
        let theirs = Event::PeerAdded {
            peer,
            initiated_by_us: false,
        };
        assert!(reply(&theirs).is_none());
        assert!(reply(&Event::Connected).is_none());
    }
}
