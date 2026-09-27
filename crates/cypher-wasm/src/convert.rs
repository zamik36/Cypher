use cypher_core::{Effect, Event, Rows, StoreOp, ui};
use js_sys::{Array, Uint8Array};
use serde::Serialize;
use wasm_bindgen::prelude::*;

// Each value is built and serialized immediately, one at a time; boxing the
// large `Event` payload would only add an allocation per effect.
#[allow(clippy::large_enum_variant)]
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
        sealed: bool,
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

pub fn to_js<T: Serialize>(value: &T) -> Result<JsValue, JsError> {
    value
        .serialize(&serde_wasm_bindgen::Serializer::new().serialize_maps_as_objects(true))
        .map_err(|e| JsError::new(&e.to_string()))
}

pub fn effects_to_js(effects: Vec<Effect>) -> Result<Array, JsError> {
    let out = Array::new();
    for effect in &effects {
        let js = match effect {
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
                if let Some(r) = reply(event) {
                    out.push(&to_js(&r)?);
                }
                match ui::event(event) {
                    Some((channel, payload)) => JsEffect::Event { channel, payload },
                    None => continue,
                }
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
                offset: *offset as f64,
                len: *len,
                headroom: u32::try_from(*headroom).unwrap_or(u32::MAX),
            },
            Effect::OpenSink {
                file_id,
                len,
                sealed,
            } => JsEffect::OpenSink {
                file_id: file_id.to_hex(),
                len: *len as f64,
                sealed: *sealed,
            },
            Effect::WriteChunk {
                file_id,
                offset,
                data,
            } => JsEffect::WriteChunk {
                file_id: file_id.to_hex(),
                offset: *offset as f64,
                data,
            },
            Effect::CloseSink { file_id, complete } => JsEffect::CloseSink {
                file_id: file_id.to_hex(),
                complete: *complete,
            },
            Effect::Disconnect { reconnect } => JsEffect::Disconnect {
                reconnect: *reconnect,
            },
        };
        out.push(&to_js(&js)?);
    }
    Ok(out)
}

/// `[[Uint8Array, Uint8Array], ...]` → owned key/value pairs.
pub fn pairs_from_js(rows: &Array) -> Result<Rows, JsError> {
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
