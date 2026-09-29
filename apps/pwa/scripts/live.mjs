// Live smoke test of the WebAssembly core over the WebSocket endpoints:
//   CYPHER_WS_GATEWAY=ws://127.0.0.1:9101 CYPHER_WS_RELAY=ws://127.0.0.1:9301 node scripts/live.mjs
// Two in-memory clients pair through a link, exchange messages both ways and
// deliver one offline message through the onion inbox.
import { readFileSync } from "node:fs";
import { initSync, Identity, Client } from "../src/wasm/cypher_wasm.js";

initSync({ module: readFileSync(new URL("../src/wasm/cypher_wasm_bg.wasm", import.meta.url)) });

const GATEWAY = process.env.CYPHER_WS_GATEWAY ?? "ws://127.0.0.1:9101";
const RELAY = process.env.CYPHER_WS_RELAY ?? "ws://127.0.0.1:9301";
const TIMEOUT_MS = 20_000;
const hex = (b) => Buffer.from(b).toString("hex");

class Peer {
  constructor(name, anonymous) {
    this.name = name;
    this.anonymous = anonymous;
    this.identity = Identity.create(name, "correct horse battery staple").intoIdentity();
    this.tables = new Map();
    this.sockets = {};
    this.waiters = new Set();
    this.log = [];
  }

  rows(table) {
    return [...(this.tables.get(table)?.values() ?? [])];
  }

  start() {
    const rows = ["meta", "peers", "outbox", "transfers"].map((t) => this.rows(t));
    this.client = new Client(this.identity, ...rows, Date.now());
    this.apply(this.client.startupEffects());
    this.apply(this.client.command({ type: "set_anonymity", require_onion: this.anonymous }, Date.now()).effects);
    this.open(
      "gateway",
      GATEWAY,
      (c, d, now) => c.frame(d, now),
      (c, now) => c.connected(now),
      (c, now) => c.disconnected(now),
    );
    this.open(
      "relay",
      RELAY,
      (c, d, now) => c.anonymousFrame(d, now),
      (c, now) => c.anonymousChannel(true, now),
      (c, now) => c.anonymousChannel(false, now),
    );
    this.ticker = setInterval(() => this.feed((c, now) => c.tick(now)), 250);
  }

  stop() {
    clearInterval(this.ticker);
    for (const ws of Object.values(this.sockets)) ((ws.onclose = null), ws.close());
    this.sockets = {};
    this.client.free();
    this.client = null;
  }

  open(name, url, onFrame, onOpen, onClose) {
    const ws = new WebSocket(url);
    ws.binaryType = "arraybuffer";
    ws.onopen = () => this.feed(onOpen);
    ws.onmessage = (m) => this.feed((c, now) => onFrame(c, new Uint8Array(m.data), now));
    ws.onclose = () => this.feed(onClose);
    this.sockets[name] = ws;
  }

  feed(call) {
    if (this.client) this.apply(call(this.client, Date.now()));
  }

  command(cmd) {
    const out = this.client.command(cmd, Date.now());
    this.apply(out.effects);
    return out;
  }

  apply(effects) {
    for (const e of effects) {
      switch (e.kind) {
        case "put": {
          const t = this.tables.get(e.table) ?? new Map();
          t.set(hex(e.key), [e.key, e.value]);
          this.tables.set(e.table, t);
          break;
        }
        case "delete":
          this.tables.get(e.table)?.delete(hex(e.key));
          break;
        case "transmit":
          this.sockets.gateway?.send(e.data);
          break;
        case "anonymous":
          this.sockets.relay?.send(e.data);
          break;
        case "event":
        case "reply":
          this.log.push(e);
          for (const w of this.waiters) w(e);
          break;
      }
    }
  }

  waitFor(what, pick) {
    const hit = this.log.map(pick).find((v) => v !== undefined);
    if (hit !== undefined) return Promise.resolve(hit);
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error(`${this.name}: timed out waiting for ${what}`)), TIMEOUT_MS);
      const w = (e) => {
        const v = pick(e);
        if (v === undefined) return;
        clearTimeout(timer);
        this.waiters.delete(w);
        resolve(v);
      };
      this.waiters.add(w);
    });
  }

  message(text) {
    return this.waitFor(`message "${text}"`, (e) =>
      e.kind === "event" && e.channel === "message" && e.payload.text === text ? e.payload : undefined,
    );
  }
}

function step(label, started) {
  console.log(`ok  ${label.padEnd(36)} ${Date.now() - started} ms`);
}

const alice = new Peer("alice", true);
const bob = new Peer("bob", false);
const t0 = Date.now();
alice.start();
bob.start();
await Promise.all(
  [alice, bob].map((p) =>
    p.waitFor("connected", (e) => (e.kind === "event" && e.channel === "connected" ? true : undefined)),
  ),
);
step("both clients authenticated", t0);

let t = Date.now();
alice.command({ type: "create_link" });
const link = await alice.waitFor("link", (e) => (e.kind === "reply" && e.op === "link_created" ? e.value : undefined));
bob.command({ type: "join_link", link });
const alicePeer = await bob.waitFor("join", (e) => (e.kind === "reply" && e.op === "joined" ? e.value : undefined));
const bobPeer = await alice.waitFor("peer", (e) =>
  e.kind === "event" && e.channel === "peer_connected" ? e.payload : undefined,
);
step("link created and joined", t);

t = Date.now();
bob.command({ type: "send_text", peer: alicePeer, text: "hello from bob" });
await alice.message("hello from bob");
alice.command({ type: "send_text", peer: bobPeer, text: "hi bob" });
await bob.message("hi bob");
step("messages both ways", t);

t = Date.now();
alice.stop();
bob.command({ type: "send_text", peer: alicePeer, text: "while you were away" });
await bob.waitFor("queued", (e) =>
  e.kind === "event" && e.channel === "message_status" && e.payload.status === "queued" ? true : undefined,
);
alice.log = [];
alice.start();
await alice.waitFor("onion", (e) =>
  e.kind === "event" && e.channel === "anonymity_level" && e.payload.level === 1 ? true : undefined,
);
alice.command({ type: "fetch_inbox" });
await alice.message("while you were away");
step("offline delivery via onion inbox", t);

alice.stop();
bob.stop();
console.log(`all passed in ${Date.now() - t0} ms`);
process.exit(0);
