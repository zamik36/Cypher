// Frame layouts against crates/cypher-wire (little-endian, no length
// prefixes) and the auth signature against Ed25519 verification.

import assert from "node:assert/strict";
import { test } from "node:test";

import * as ed from "@noble/ed25519";

import * as p from "./protocol.js";

const peer = new Uint8Array(32).fill(7);

test("hello carries version 2 and the peer id", () => {
  const bytes = new Uint8Array(p.hello(0x01020304, peer));
  assert.deepEqual(Array.from(bytes.subarray(0, 7)), [0x01, 4, 3, 2, 1, 2, 0]);
  assert.deepEqual(bytes.subarray(7), peer);
  assert.equal(bytes.length, 39);
});

test("send lays out recipient, ack flag and raw body", () => {
  const body = Uint8Array.of(9, 8, 7);
  const bytes = new Uint8Array(p.send(5, peer, body, true));
  assert.equal(bytes.length, 5 + 32 + 1 + 3);
  assert.deepEqual(Array.from(bytes.subarray(0, 5)), [0x10, 5, 0, 0, 0]);
  assert.equal(bytes[37], 1);
  assert.deepEqual(Array.from(bytes.subarray(38)), [9, 8, 7]);
  assert.equal(new Uint8Array(p.send(5, peer, body, false))[37], 0);
});

test("auth signs the context and nonce with the identity key", () => {
  const id = p.identity();
  const nonce = new Uint8Array(32).fill(3);
  const bytes = new Uint8Array(p.auth(id.secret, nonce));
  assert.equal(bytes.length, 69);
  assert.equal(bytes[0], p.Kind.Auth);
  const signed = new Uint8Array([...Array.from("cypher-session-auth-v2", (c) => c.charCodeAt(0)), ...nonce]);
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
  const recv = new Uint8Array(5 + 32 + body.length);
  recv[0] = p.Kind.Recv;
  recv.set(body, 37);
  assert.equal(p.stampOf(p.recvBody(p.decode(recv.buffer).fields)), 1234.5);
  assert.equal(p.stamped(2, 1).length, 8, "room for the stamp");
});
