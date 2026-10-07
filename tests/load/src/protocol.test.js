// Frame layouts against crates/cypher-wire (little-endian, no length
// prefixes) and the auth signature against Ed25519 verification.

import assert from "node:assert/strict";
import { test } from "node:test";

import * as ed from "@noble/ed25519";

import * as p from "./protocol.js";

const peer = new Uint8Array(32).fill(7);

test("hello carries version 3, the peer id and the device", () => {
  const bytes = new Uint8Array(p.hello(0x01020304, peer));
  assert.deepEqual(Array.from(bytes.subarray(0, 7)), [0x01, 4, 3, 2, 1, 3, 0]);
  assert.deepEqual(bytes.subarray(7, 39), peer);
  assert.deepEqual(Array.from(bytes.subarray(39)), [1, 0, 0, 0]);
  assert.equal(bytes.length, 43);
});

test("send lays out recipient, device, ack flag and raw body", () => {
  const body = Uint8Array.of(9, 8, 7);
  const bytes = new Uint8Array(p.send(5, peer, body, true));
  assert.equal(bytes.length, 5 + 32 + 4 + 1 + 3);
  assert.deepEqual(Array.from(bytes.subarray(0, 5)), [0x10, 5, 0, 0, 0]);
  assert.deepEqual(Array.from(bytes.subarray(37, 41)), [1, 0, 0, 0]);
  assert.equal(bytes[41], 1);
  assert.deepEqual(Array.from(bytes.subarray(42)), [9, 8, 7]);
  assert.equal(new Uint8Array(p.send(5, peer, body, false))[41], 0);
});

test("auth signs the context, nonce and device with the identity key", () => {
  const id = p.identity();
  const nonce = new Uint8Array(32).fill(3);
  const bytes = new Uint8Array(p.auth(id.secret, nonce));
  assert.equal(bytes.length, 69);
  assert.equal(bytes[0], p.Kind.Auth);
  const context = Array.from("cypher-session-auth-v3", (c) => c.charCodeAt(0));
  const signed = new Uint8Array([...context, ...nonce, 1, 0, 0, 0]);
  assert.ok(ed.verify(bytes.subarray(5), signed, id.peer));
  assert.ok(!ed.verify(bytes.subarray(5), new Uint8Array(54), id.peer));
});

test("server frames decode header and fields", () => {
  const ack = Uint8Array.of(0x12, 9, 0, 0, 0, p.Delivery.Busy).buffer;
  const frame = p.decode(ack);
  assert.equal(frame.kind, p.Kind.SendAck);
  assert.equal(frame.reqId, 9);
  assert.equal(frame.fields[0], p.Delivery.Busy);
  assert.equal(p.decode(new ArrayBuffer(2)).kind, -1);
});

test("stamped payload round-trips through a recv frame", () => {
  const body = p.stamped(64, 1234.5);
  assert.equal(body.length, 64);
  const recv = new Uint8Array(5 + 32 + 4 + body.length);
  recv[0] = p.Kind.Recv;
  recv.set(body, 41);
  assert.equal(p.stampOf(p.recvBody(p.decode(recv.buffer).fields)), 1234.5);
  assert.equal(p.stamped(2, 1).length, 8, "room for the stamp");
});
