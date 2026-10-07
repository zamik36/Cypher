// The gateway's wire protocol as a load client speaks it: one binary
// WebSocket message per frame, `[kind u8][req_id u32 LE][fields]`, all
// integers little-endian (crates/cypher-wire/src/message.rs).

import * as ed from "@noble/ed25519";
import { sha512 } from "@noble/hashes/sha2.js";

ed.hashes.sha512 = sha512;

export const PROTOCOL_VERSION = 3;

export const Kind = Object.freeze({
  Hello: 0x01,
  Challenge: 0x02,
  Auth: 0x03,
  Ready: 0x04,
  Ping: 0x05,
  Pong: 0x06,
  Superseded: 0x07,
  Send: 0x10,
  Recv: 0x11,
  SendAck: 0x12,
  Error: 0x7f,
});

export const Delivery = Object.freeze({ Delivered: 0, Offline: 1, Busy: 2 });

const HEADER = 5;
const PEER = 32;
const DEVICE = 4;
/// Load clients are each an identity's first and only device.
export const FIRST_DEVICE = 1;
/// `b"cypher-session-auth-v3"`, signed together with the server's nonce and
/// the device.
const AUTH_CONTEXT = ascii("cypher-session-auth-v3");

/// k6's JS engine cannot build typed arrays from strings (no string
/// iterators in `TypedArray.from`), so bytes are copied explicitly.
function ascii(text) {
  const bytes = new Uint8Array(text.length);
  for (let i = 0; i < text.length; i += 1) {
    bytes[i] = text.charCodeAt(i);
  }
  return bytes;
}

/// A fresh identity: the peer id is the Ed25519 public key.
export function identity() {
  const { secretKey, publicKey } = ed.keygen();
  return { secret: secretKey, peer: publicKey };
}

function frame(kind, reqId, size) {
  const bytes = new Uint8Array(HEADER + size);
  bytes[0] = kind;
  new DataView(bytes.buffer).setUint32(1, reqId >>> 0, true);
  return bytes;
}

export function hello(reqId, peer) {
  const bytes = frame(Kind.Hello, reqId, 2 + PEER + DEVICE);
  const view = new DataView(bytes.buffer);
  view.setUint16(HEADER, PROTOCOL_VERSION, true);
  bytes.set(peer, HEADER + 2);
  view.setUint32(HEADER + 2 + PEER, FIRST_DEVICE, true);
  return bytes.buffer;
}

/// Proof of possession of `secret` for the server's challenge `nonce`.
export function auth(secret, nonce) {
  const signed = new Uint8Array(AUTH_CONTEXT.length + nonce.length + DEVICE);
  signed.set(AUTH_CONTEXT);
  signed.set(nonce, AUTH_CONTEXT.length);
  new DataView(signed.buffer).setUint32(AUTH_CONTEXT.length + nonce.length, FIRST_DEVICE, true);
  const bytes = frame(Kind.Auth, 0, 64);
  bytes.set(ed.sign(signed, secret), HEADER);
  return bytes.buffer;
}

/// A `Send` to the first device of `to`.
export function send(reqId, to, body, wantAck) {
  const bytes = frame(Kind.Send, reqId, PEER + DEVICE + 1 + body.length);
  bytes.set(to, HEADER);
  new DataView(bytes.buffer).setUint32(HEADER + PEER, FIRST_DEVICE, true);
  bytes[HEADER + PEER + DEVICE] = wantAck ? 1 : 0;
  bytes.set(body, HEADER + PEER + DEVICE + 1);
  return bytes.buffer;
}

export function ping(reqId) {
  return frame(Kind.Ping, reqId, 0).buffer;
}

/// Splits a server frame; `fields` is everything after the header.
export function decode(buffer) {
  const bytes = new Uint8Array(buffer);
  if (bytes.length < HEADER) {
    return { kind: -1, reqId: 0, fields: new Uint8Array(0) };
  }
  return {
    kind: bytes[0],
    reqId: new DataView(buffer).getUint32(1, true),
    fields: bytes.subarray(HEADER),
  };
}

/// Body of a `Recv` (after the sender's peer id and device).
export function recvBody(fields) {
  return fields.subarray(PEER + DEVICE);
}

/// A payload of `size` bytes whose first 8 carry `stamp` (ms, f64 LE).
export function stamped(size, stamp) {
  const body = new Uint8Array(Math.max(size, 8));
  new DataView(body.buffer).setFloat64(0, stamp, true);
  return body;
}

export function stampOf(body) {
  return new DataView(body.buffer, body.byteOffset, body.byteLength).getFloat64(0, true);
}
