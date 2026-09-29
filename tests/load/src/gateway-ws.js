// k6 load on the gateway's WebSocket listener, the path browsers use.
//
// Each pair is two fresh identities: the sender on one gateway node, the
// receiver on another (so frames cross NATS). A VU drives PAIRS_PER_VU pairs
// on its event loop: a k6 VU is a whole JS runtime, so thousands of VUs
// would exhaust the load generator long before the gateway. The sender relays a
// time-stamped `Send` at MSG_RATE; the scenario measures
//   connect   socket open → `Ready` (TLS-less WS + Hello/Challenge/Auth)
//   ack_rtt   `Send` → the sender's `SendAck`
//   delivery  `Send` → the receiver's `Recv` (end to end, same clock)
// and fails on any error, Offline/Busy ack or a threshold breach.
//
// Environment: GATEWAY_WS, PEER_GATEWAY_WS, PAIRS (total), PAIRS_PER_VU,
// RAMP, HOLD, SESSION (s), MSG_RATE (per second), PAYLOAD (bytes), P99_MS.

import { check } from "k6";
import { Counter, Trend } from "k6/metrics";
import { clearInterval, setInterval, setTimeout } from "k6/timers";
import { WebSocket } from "k6/websockets";

import * as wire from "./protocol.js";

const env = (name, fallback) => __ENV[name] ?? fallback;
const SENDER_URL = env("GATEWAY_WS", "ws://127.0.0.1:9101");
const RECEIVER_URL = env("PEER_GATEWAY_WS", SENDER_URL);
const PAIRS = Number(env("PAIRS", 10));
const PAIRS_PER_VU = Number(env("PAIRS_PER_VU", 10));
const VUS = Math.max(1, Math.ceil(PAIRS / PAIRS_PER_VU));
/// A VU opens its pairs this far apart rather than signing all at once.
const PAIR_STAGGER_MS = 20;
const RAMP = env("RAMP", "10s");
const HOLD = env("HOLD", "20s");
const SESSION_MS = Number(env("SESSION", 20)) * 1000;
const MSG_INTERVAL_MS = 1000 / Number(env("MSG_RATE", 5));
const PAYLOAD = Number(env("PAYLOAD", 256));
const P99_MS = Number(env("P99_MS", 50));
/// Idle connections are closed after 60 s; stay well inside it.
const PING_MS = 30_000;

const connect = new Trend("connect", true);
const ackRtt = new Trend("ack_rtt", true);
const delivery = new Trend("delivery", true);
const relayed = new Counter("relayed");
const unacked = new Counter("unacked");
const errors = new Counter("errors");
const offline = new Counter("offline");
const busy = new Counter("busy");

export const options = {
  scenarios: {
    pairs: {
      executor: "ramping-vus",
      startVUs: 0,
      stages: [
        { duration: RAMP, target: VUS },
        { duration: HOLD, target: VUS },
      ],
      gracefulRampDown: `${SESSION_MS / 1000 + 10}s`,
      gracefulStop: `${SESSION_MS / 1000 + 10}s`,
    },
  },
  thresholds: {
    ack_rtt: [`p(99)<${P99_MS}`],
    delivery: [`p(99)<${P99_MS}`],
    checks: ["rate==1"],
    errors: ["count==0"],
    offline: ["count==0"],
    busy: ["count==0"],
    unacked: ["count==0"],
  },
  summaryTrendStats: ["avg", "min", "med", "p(90)", "p(99)", "p(99.9)", "max"],
};

/// Opens an authenticated client; `onReady` runs once `Ready` arrives,
/// every later frame goes to `onFrame`.
function client(url, id, onReady, onFrame) {
  const ws = new WebSocket(url);
  ws.binaryType = "arraybuffer";
  const opened = Date.now();
  let ready = false;
  ws.onopen = () => ws.send(wire.hello(1, id.peer));
  ws.onmessage = (event) => {
    const frame = wire.decode(event.data);
    if (ready) {
      onFrame(frame);
    } else if (frame.kind === wire.Kind.Challenge) {
      ws.send(wire.auth(id.secret, frame.fields.subarray(0, 32)));
    } else if (frame.kind === wire.Kind.Ready) {
      ready = true;
      connect.add(Date.now() - opened);
      onReady();
    } else {
      errors.add(1, { stage: "auth", kind: String(frame.kind) });
      ws.close();
    }
  };
  ws.onerror = () => errors.add(1, { stage: "socket" });
  return {
    ws,
    isReady: () => ready,
  };
}

export default function vu() {
  const pairs = Math.min(PAIRS_PER_VU, PAIRS);
  for (let i = 0; i < pairs; i += 1) {
    setTimeout(pair, i * PAIR_STAGGER_MS);
  }
}

function pair() {
  const [sender, receiver] = [wire.identity(), wire.identity()];
  const pending = new Map();
  let reqId = 1;
  let closing = false;
  const timers = [];

  const rx = client(RECEIVER_URL, receiver, start, (frame) => {
    if (frame.kind === wire.Kind.Recv) {
      delivery.add(Date.now() - wire.stampOf(wire.recvBody(frame.fields)));
    }
  });
  const tx = client(SENDER_URL, sender, start, (frame) => {
    if (frame.kind !== wire.Kind.SendAck) {
      return;
    }
    const sentAt = pending.get(frame.reqId);
    pending.delete(frame.reqId);
    if (sentAt === undefined) {
      errors.add(1, { stage: "relay", reason: "stray ack" });
      return;
    }
    ackRtt.add(Date.now() - sentAt);
    relayed.add(1);
    const status = frame.fields[0];
    if (status === wire.Delivery.Offline) offline.add(1);
    if (status === wire.Delivery.Busy) busy.add(1);
  });
  for (const side of [tx, rx]) {
    side.ws.onclose = () => {
      if (!closing) errors.add(1, { stage: "relay", reason: "closed by server" });
    };
  }

  /// Relaying starts once both ends are authenticated.
  function start() {
    if (!tx.isReady() || !rx.isReady()) return;
    timers.push(
      setInterval(() => {
        reqId += 1;
        const now = Date.now();
        pending.set(reqId, now);
        tx.ws.send(wire.send(reqId, receiver.peer, wire.stamped(PAYLOAD, now), true));
      }, MSG_INTERVAL_MS),
      setInterval(() => {
        tx.ws.send(wire.ping(0));
        rx.ws.send(wire.ping(0));
      }, PING_MS),
    );
  }

  setTimeout(() => {
    closing = true;
    timers.forEach(clearInterval);
    check(null, { "both ends authenticated": () => tx.isReady() && rx.isReady() });
    // Acks still in flight when the session ends are not lost ones: give
    // them a moment before closing.
    setTimeout(() => {
      unacked.add(pending.size);
      tx.ws.close();
      rx.ws.close();
    }, 1000);
  }, SESSION_MS);
}
