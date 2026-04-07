# Onion Routing for Inbox Anonymity — Production Plan

## Implementation Status — 2026-04-07

This document started as a design plan. The current codebase now implements the core production-safe parts of the plan with a few scope adjustments:

- transport bootstrap is discovered from signaling, not from new client env variables
- relay and signaling keys are persisted locally on disk, not generated from env seeds
- Tor bridge configuration is a runtime app setting, not an env contract
- inbox responses are signed by signaling and verified by the client before `InboxAck`
- inbox fetch uses a two-phase claim/ack flow with Redis-backed recovery for missed ACKs
- anonymous transport code was decomposed into focused client and service modules

Implemented key paths:

- `crates/cypher-client-core/src/onion/*`
- `crates/cypher-client-core/src/api/*`
- `services/signaling/src/{bootstrap,inbox,peer,delivery,signing,key_store,stun}.rs`
- `services/relay/src/{main,onion,identity}.rs`

Production key files:

- `data/relay/onion_identity.bin`
- `data/signaling/inbox_signing.bin`
- `data/signaling/inbox_hmac.bin`

Out of current scope or intentionally deferred:

- automatic bridge-provider HTTP flows
- broader organic relay / DHT architecture
- full end-to-end NAT/Redis/NATS integration testbed for anonymous transport

## Problem

Even with per-peer inbox IDs and E2EE inbox exchange, the server learns the
mapping **identity → set of inbox IDs** when the client sends `InboxFetchBatch`.
The gateway sees `session_id` (= knows `peer_id`) and the list of `inbox_id`s
being fetched.

## Goal

The client should be able to fetch (and store) inbox messages without the server
knowing **which identity** is behind the request.

## Infrastructure

- **Server 1 (main):** Gateway + Signaling + Redis + NATS (existing)
- **Server 2 (relay):** `cypher-relay` binary (lightweight, ~50MB RAM)
- **Tor network:** Used as anonymous transport when available (no cost, no infra)

---

## Architecture: 3-Tier Anonymous Fetch

The client chooses the best available transport for anonymous inbox fetch,
in order of preference:

### Tier 1: Tor Transport (strongest anonymity)

```
Alice          Tor (3 hops)           Server 1
  │                │                     │
  │── Shadow TLS ──┼── TLS ────────────>│
  │   (via Tor)    │                     │── InboxFetch(X)
  │                │                     │
  │<── response ───┼─────────────────────│
```

Alice opens a **shadow session** (ephemeral peer_id) through a Tor circuit
directly to the gateway. No custom relay needed — Tor provides 3 independent
hops operated by thousands of volunteers worldwide.

- Server sees: Tor exit IP + random ephemeral peer_id → InboxFetch(X)
- Server **cannot** link this to Alice's real session
- Tor exit sees: encrypted TLS traffic to gateway IP (cannot read content)
- **No single node has the full picture**

**When to use:** Desktop (always available), mobile foreground (if Tor proxy running).

### Tier 2: 1-Hop Relay via Server 2 (good anonymity)

```
Alice          Server 2 (relay)       Server 1
  │                │                     │
  │── Shadow TLS ─>│                     │
  │  (ephemeral    │── InboxFetch(X) ──>│
  │   peer_id)     │                     │
  │                │<── response ────────│
  │<── response ───│                     │
```

Alice opens a shadow session to the gateway, sends a `ChatSend` to the relay
on Server 2. The relay decrypts the onion layer and sends `InboxFetch` to the
server on Alice's behalf.

- Server 1 sees: Relay requested inbox X (doesn't know it's for Alice)
- Server 2 (relay) sees: someone with ephemeral peer_id asked for inbox X
  (doesn't know it's Alice — shadow session hides real peer_id)
- **Risk:** If Server 1 + Server 2 are both compromised → Alice exposed.
  Since both are controlled by you, this is "trust the operator" model.

**When to use:** Tor unavailable, or mobile where Tor is impractical.

### Tier 3: Direct Fetch with Hardening (minimal anonymity)

```
Alice                                 Server 1
  │                                      │
  │── InboxFetch(X) ───────────────────>│
  │<── response ────────────────────────│
```

Current behavior, but with padding + cover traffic + individual fetch
(not batched). Server knows Alice's inbox IDs, but analysis is harder.

**When to use:** All relays offline, Tor blocked, or user chose battery-saver mode.

### Tier Selection Logic

```rust
fn select_transport() -> Transport {
    if tor_proxy.is_connected() {
        Transport::Tor           // Level 3: strongest
    } else if relay_circuit.is_ready() {
        Transport::Relay         // Level 2: good
    } else if relay_circuit.is_warming() {
        // Wait up to 2s for circuit, then fall back
        Transport::RelayOrDirect // Level 2 → Level 0
    } else {
        Transport::Direct        // Level 0: hardened but not anonymous
    }
}
```

---

## Threat Model

### Adversary Capabilities

| Adversary | Capabilities |
|-----------|-------------|
| **Passive server** | Observes all traffic: timing, sizes, session↔peer_id mapping, inbox access patterns |
| **Active server** | Above + injects/drops messages, creates fake relay identities (Sybil) |
| **Server 2 compromised** | Sees onion layer: ephemeral peer_id + inbox_id |
| **Server 1 + Server 2 collude** | Full picture: can link Alice to inbox (Tier 2 only) |
| **Tor exit + Server collude** | Cannot link — Tor exit sees only TLS ciphertext, not inbox_id |
| **Global passive adversary** | Sees all network traffic (ISP-level) |

### Protection by Tier

| Threat | Tier 1 (Tor) | Tier 2 (Relay) | Tier 3 (Direct) |
|--------|-------------|----------------|-----------------|
| Server links identity → inbox | **Protected** | **Protected** | Exposed |
| Server + Relay collude | **Protected** (Tor is independent) | Exposed (same operator) | N/A |
| Timing correlation | **Strong** (Tor adds noise) | Medium (jitter) | Weak |
| Traffic analysis | **Strong** (Tor cover) | Medium (our cover traffic) | Weak (padding only) |
| Relay tampers with response | N/A (no relay) | Detected (server_sig) | N/A |
| Message loss on relay failure | N/A | **Protected** (two-phase fetch) | N/A |

### Attacks & Mitigations

| Attack | Mitigation |
|--------|------------|
| **Timing correlation** | Tor (Tier 1) / random exponential jitter (Tier 2) / cover traffic |
| **Size analysis** | Fixed-size padding to bucket sizes (all tiers) |
| **Relay fingerprinting** | Cover fetches: all clients periodically fetch random dummy inboxes |
| **Active MITM relay** | Server signs inbox responses with Ed25519 key |
| **Sybil relay** | Only 1 infra relay (Server 2), pinned key. Future organic relays use DHT + identity-age |
| **Replay attack** | Unique circuit_id + monotonic nonce per request |
| **Relay refuses** | Timeout (5s) → fallback to Tor or direct |
| **Tor blocked** | Tor bridges (obfs4/snowflake) → Tier 2 (relay) → Tier 3 (direct) |
| **Intersection attack** | Rotate shadow sessions frequently + cover traffic |

---

## Protocol Design

### Wire Format

All onion messages are embedded inside regular `ChatSend` ciphertext as control
messages (prefix `0x02`), making them **indistinguishable from chat** at the
gateway level. No new proto constructors are visible to the server.

```
Decrypted payload format:

[0x02] [1 byte subtype] [payload...]

Subtypes:
  0x01 = relay_forward  — (reserved for future 2-hop)
  0x02 = relay_request  — "send this payload to the server, return response"
  0x03 = relay_response — "here is the server's response to your relay request"
```

### Shadow Sessions (Both Tiers)

**Problem:** The gateway rewrites `ChatSend.peer_id` from target → sender.
If Alice uses her real session, the relay (or gateway via Tor) knows her peer_id.

**Solution:** Ephemeral throwaway identity for anonymous operations.

```
Shadow session lifecycle:

1. Alice generates fresh ephemeral keypair: shadow_ek
2. Alice opens a NEW gateway connection:
   - Tier 1: through Tor SOCKS proxy → gateway
   - Tier 2: directly to gateway (different TLS connection)
3. SESSION_INIT with client_id = shadow_ek.public (random, unlinkable)
4. No prekeys uploaded (shadow identity is not discoverable)
5. All anonymous inbox fetches go through this session
6. Destroyed after use (max 10 min or batch complete)

What each party sees:
  Gateway: "ephemeral peer {random_id} connected from {tor_exit_ip | alice_ip}"
  Relay (Tier 2): "peer {random_id} asked me to fetch inbox X"
  Neither can link shadow_ek to Alice's real peer_id
```

**Tier 1 IP protection:** Shadow session goes through Tor → gateway sees Tor exit
IP, not Alice's real IP. Full unlinkability.

**Tier 2 IP limitation:** Shadow session connects directly → gateway sees Alice's
IP on both sessions. Mitigation: gateway cannot *prove* they belong to the same
user (different peer_id, different session_id), but IP analysis is possible.

### Onion Encryption (Tier 2: 1-hop)

```
// Alice builds the onion for relay on Server 2:

payload = AEAD_Encrypt(
    key  = circuit_key,
    data = {
        subtype: relay_request,
        circuit_id: [16 bytes],
        request_payload: InboxFetch(X).serialize(),
        response_mac_key: [32 bytes],
    },
    aad = circuit_id,
)

// Sent as ChatSend to Relay via shadow session (prefix 0x02)
final_payload = [0x02] || payload
```

### Tor Transport (Tier 1: no onion layer needed)

```
// Alice sends InboxFetch DIRECTLY through Tor shadow session.
// No custom encryption layer — TLS + Tor provides 3 hops.

shadow_session.send(InboxFetch(X).serialize())

// Server sees: random ephemeral peer from Tor exit → InboxFetch(X)
// Cannot link to Alice. Tor provides the anonymity layer.
```

### Circuit Key Establishment (Tier 2, 0-RTT)

```
Circuit setup:

1. Alice generates ephemeral X25519 keypair (circuit_ek)
2. Alice knows relay's circuit_spk (hardcoded — only 1 relay)
3. DH: circuit_shared = X25519(circuit_ek_secret, relay_circuit_spk)
4. circuit_key = HKDF-SHA256(circuit_shared, info="cypher-circuit-v1")
5. circuit_id = random 16 bytes
6. Alice embeds circuit_ek.public in first message
7. Relay derives same key on receipt → 0-RTT
```

### Circuit / Session Lifetime & Pre-warming

- Max 30 requests per circuit (Tier 2) or shadow session (Tier 1)
- Max 10 minutes TTL
- Relay zeroizes circuit keys on close or timeout

**Pre-warming:** Client maintains **multiple parallel transports** in background:

```
TransportPool {
    // Tier 1: 2-3 independent Tor shadow sessions (different circuits/exit IPs)
    tor_sessions: Vec<ShadowSession>,       // target: 3

    // Tier 2: 2-3 independent relay circuits (different shadow sessions)
    relay_circuits: Vec<Circuit>,            // target: 3

    warming: Vec<PendingTransport>,          // being established in background

    // Background task:
    // - Maintain 3 ready transports of the best available tier
    // - Each transport uses a DIFFERENT shadow session (different ephemeral peer_id)
    // - Tor: each session uses a different Tor circuit (different exit IP)
    // - Relay: each session is a separate TLS connection with unique peer_id
    //   (relay cannot correlate sessions — different peer_ids)
    // - Rotate before TTL expires (max 30 requests or 10 min)
}
```

**Why 3 parallel transports:** Distributing 20+ inbox fetches across 3
independent sessions gives ~3x speedup (parallel pipeline). Each transport
has a different ephemeral peer_id, so neither the relay nor the server can
tell they belong to the same user.

**Tor: 3 sessions = 3 different exit IPs.** Server sees 3 unrelated peers
from 3 different Tor exits. Cannot correlate.

**Relay: 3 shadow sessions = 3 different peer_ids.** Relay sees 3 unrelated
peers asking for different inbox subsets. Cannot correlate (each shadow
session is independent, different TLS connection, different ephemeral key).

---

## Latency Optimization

### Pipelined Parallel Fetch

Inbox_ids are fetched individually (not batched), distributed across **3
parallel transports**, pipelined within each, with dummy padding.

**Jitter model — random exponential (anti-fingerprinting):**

```rust
fn next_jitter() -> Duration {
    // Truncated exponential: mean=100ms, min=20ms, max=500ms
    let raw = -100.0 * ln(1.0 - random::<f64>());
    let clamped = raw.clamp(20.0, 500.0);
    Duration::from_millis(clamped as u64)
}

fn pipeline_schedule(n_requests: usize) -> Vec<Duration> {
    let mut delays: Vec<_> = (0..n_requests).map(|_| next_jitter()).collect();
    // Insert 1-2 long pauses at random positions (anti-fingerprint)
    let pause_count = thread_rng().gen_range(1..=2);
    for _ in 0..pause_count {
        let pos = thread_rng().gen_range(0..n_requests);
        delays[pos] += Duration::from_millis(thread_rng().gen_range(300..1000));
    }
    delays
}
```

### Smart Batching Rules

```rust
fn distribute_fetches(inbox_ids: &[InboxId], transports: &[Transport]) -> Vec<FetchPlan> {
    let per_transport = (inbox_ids.len() + transports.len() - 1) / transports.len();
    let target = per_transport + 2;  // +2 dummies minimum

    transports.iter().enumerate().map(|(i, transport)| {
        let real = &inbox_ids[i*per_transport .. min((i+1)*per_transport, inbox_ids.len())];
        let dummies: Vec<_> = (0..target - real.len()).map(|_| random_inbox_id()).collect();

        FetchPlan {
            transport,
            inbox_ids: real.iter().chain(&dummies).shuffled().collect(),
            jitter_schedule: pipeline_schedule(target),
        }
    }).collect()
}
```

### Tunable Speed vs Anonymity

Users choose via settings:
- **Max anonymity:** 1-2 pauses per transport (default)
- **Balanced:** 0-1 pauses
- **Max speed:** No pauses, reduced jitter range 20-200ms

---

## Padding & Cover Traffic

### Padding (mandatory, day-1)

All `ChatSend` payloads MUST be padded to fixed bucket sizes before encryption:

```
PADDING_BUCKETS = [512, 1024, 2048, 4096, 8192]

fn pad(plaintext: &[u8]) -> Vec<u8> {
    let bucket = PADDING_BUCKETS
        .iter()
        .find(|&&b| b >= plaintext.len() + 2)
        .unwrap_or(&8192);
    let mut padded = Vec::with_capacity(*bucket);
    padded.extend_from_slice(&(plaintext.len() as u16).to_le_bytes());
    padded.extend_from_slice(plaintext);
    padded.resize(*bucket, 0);
    padded
}
```

### Cover Traffic (mandatory, day-1)

```
Cover fetch schedule:
- Every 2-5 minutes (random interval): fetch 1-3 random dummy inbox_ids
- Fetches go through the current best transport (Tor > Relay > Direct)
- Dummy inbox_ids are randomly generated (server returns empty)
- Client discards empty responses silently

Cover chaff:
- Every 1-3 minutes: send a dummy ChatSend to a random online peer
- Payload is padded chaff (prefix 0x00 = discard, peer drops silently)
```

### Power-Aware Mode (Mobile)

```
Desktop mode (default):
  Tor: persistent connection
  Cover fetches: every 2-5 min
  Cover chaff: every 1-3 min

Mobile foreground:
  Tor: optional (if tor proxy available)
  Cover fetches: every 5-10 min (reduced)
  Cover chaff: every 3-5 min
  Transport pool: 1 ready

Mobile background:
  All cover traffic: OFF (OS suspends app)
  Transport pool: torn down
  Fetch on wake-up: cold-start transport

Mobile battery-saver (user opt-in):
  Fetch: DIRECT (non-anonymous, Level 0)
  User warned via Anonymity Level Indicator
```

**Data budget (mobile foreground):** ~60KB/hour.

---

## Response Integrity

### Server-Signed Inbox Responses

```
InboxFetchResponse {
    messages: Vec<Bytes>,
    count: u32,
    inbox_id_hash: H(inbox_id),
    timestamp: u64,
    server_sig: Ed25519(server_key, messages || count || inbox_id_hash || timestamp),
}
```

Alice verifies `server_sig` after decryption. If verification fails →
tear down transport and rebuild.

**Server signing key:** pinned in client binary, rotated quarterly.

### Two-Phase Fetch (Claim + ACK)

Prevents message loss when relay or Tor circuit fails mid-response.

```
Phase 1 — Claim:
  Client sends: InboxFetch(inbox_id)
  Server does:
    messages = LRANGE inbox:{id} 0 -1
    SET inbox:{id}:claimed = messages       (backup, 5 min TTL)
    DEL inbox:{id}                          (clear main queue)
    Return: InboxFetchResponse(messages, claim_token, server_sig)

Phase 2 — Acknowledge:
  Client sends: InboxAck(inbox_id, claim_token)
  Server does:
    DEL inbox:{id}:claimed                  (permanent delete)

Timeout recovery (5 min, no ACK received):
  Server restores: RPUSH inbox:{id} ...claimed_messages
```

**claim_token** = HMAC-SHA256(server_secret, inbox_id || timestamp).

**Proto addition:**
```
@0xC2000005 inbox.ack inbox_id:Bytes claim_token:Bytes = inbox.Ok;
```

---

## Anonymity Level Indicator

The client MUST display the current anonymity level to the user.

```
Level 3 (Strong):   Tor transport active
  → "Inbox fetch routed through Tor (3 independent relays)"
  → Green shield icon

Level 2 (Standard): Relay transport via Server 2
  → "Inbox fetch routed through relay"
  → Yellow shield icon

Level 1 (Degraded): Relay + organic (future, both from same trust domain)
  → "⚠ Limited relay diversity"
  → Orange shield icon

Level 0 (Direct):   No anonymous transport available
  → "⚠ Direct fetch — server can see your inbox IDs. Messages still E2EE."
  → Red shield icon, auto-retry transport in background
```

---

## Tor Integration Details

### Embedded vs External Tor

| Approach | Pros | Cons | Recommended |
|----------|------|------|-------------|
| **Embedded `arti`** (Rust Tor client) | No external deps, ships with app | +5MB binary, startup 5-15s | **Yes (desktop)** |
| **System Tor proxy** (SOCKS5) | Already installed on some systems | User must install Tor | Yes (fallback) |
| **Orbot** (Android) | Standard Android Tor proxy | User must install | Yes (mobile Android) |
| **Not available** (iOS) | — | Apple restricts Tor on iOS | Fall back to Tier 2 |

### `arti` Integration

[arti](https://gitlab.torproject.org/tpo/core/arti) is the official Rust
Tor implementation by the Tor Project. It provides:

```rust
// Establish Tor-backed TLS connection to gateway:

let tor_client = arti_client::TorClient::create_bootstrapped(config).await?;
let tor_stream = tor_client.connect(("gateway.example.com", 443)).await?;
let tls_stream = tls_connector.connect("gateway.example.com", tor_stream).await?;

// Now use tls_stream as a normal TransportSession:
let shadow_session = TransportSession::from_stream(tls_stream);
shadow_session.send(session_init(ephemeral_peer_id, nonce)).await?;
shadow_session.send(inbox_fetch(inbox_id)).await?;
```

### Tor Bootstrap Latency

| Phase | Time | Notes |
|-------|------|-------|
| First launch (build circuits) | 10-30s | One-time, cached after |
| Subsequent launches (cached) | 2-5s | Reads cached consensus |
| New circuit (pre-warmed) | 1-3s | Background rotation |
| Request through existing circuit | 200-600ms | Per InboxFetch RTT |

**Mitigation:** Start Tor bootstrap on app launch (background). By the time
user needs inbox fetch, circuits are ready.

### Tor Bridges (Censorship Circumvention)

In countries where Tor is blocked (Russia, China, Iran), direct connections
to Tor relays are filtered by DPI (ТСПУ in Russia). **Tor bridges** bypass
this by connecting through unlisted relay addresses with traffic obfuscation.

**Supported bridge types:**

| Type | How it works | Effectiveness |
|------|-------------|---------------|
| **obfs4** | Transforms Tor traffic to look like random noise | High (defeats most DPI) |
| **snowflake** | Routes through WebRTC proxies (volunteers' browsers) | High (hard to block without breaking WebRTC) |
| **meek** | Disguises traffic as HTTPS to cloud providers (Azure, CDN) | Very high (blocking = blocking Azure) |
| **Manual bridges** | User enters specific relay IP:port + fingerprint | Depends on relay availability |

**Integration with arti:**

```rust
let mut config = TorClientConfigBuilder::default();

// Option 1: Built-in bridges (shipped with app, updated via app updates)
config.bridges().bridges([
    "obfs4 198.51.100.1:443 <fingerprint> cert=<cert> iat-mode=0",
    "snowflake 192.0.2.3:1 <fingerprint> ...",
]);

// Option 2: User-provided bridges (settings UI)
// User pastes bridge lines from https://bridges.torproject.org
// or from community sources like torscan-ru.ntc.party
for bridge_line in user_settings.custom_bridges {
    config.bridges().bridges([bridge_line]);
}

// Option 3: Request bridges automatically via moat (Tor's bridge distributor)
// arti supports this via BridgeProvider
```

**Settings UI:**

```
Tor Configuration:
  ○ Connect directly (default)
  ○ Use built-in bridges (if Tor is blocked in your country)
  ○ Use custom bridges:
    ┌─────────────────────────────────────────────────┐
    │ Paste bridge lines here:                         │
    │ obfs4 79.250.68.210:9001 1BAFC3BF... cert=...   │
    │ obfs4 195.52.167.216:9001 70805F15... cert=...   │
    └─────────────────────────────────────────────────┘
    Get bridges: https://bridges.torproject.org
```

**Fallback chain with bridges:**

```
1. Try direct Tor connection (2-5s)
2. If blocked → try built-in obfs4 bridges (5-15s)
3. If blocked → try snowflake (5-20s)
4. If all Tor options fail → fall back to Tier 2 (relay on Server 2)
5. If relay unavailable → Tier 3 (direct with padding)

Each step happens in background. Relay is available immediately
while Tor/bridges bootstrap.
```

### Tor Availability Fallback Chain

```
App launch:
  1. Start arti bootstrap (background, non-blocking)
     - If bridges configured: use bridge transport
  2. Establish 3 relay circuits to Server 2 (fast, ~200ms each)
  3. First inbox fetch: use relay (immediately available)
  4. When Tor ready (2-30s): open 3 Tor shadow sessions, switch to Tor
  5. If Tor fails after 60s: stay on relay

Steady state:
  Tor available → 3 Tor sessions (Level 3)
  Tor down → 3 relay circuits (Level 2)
  Relay down → direct with padding (Level 0)
```

---

## Components to Build

### New Crates / Modules

| Component | Location | Description | Est. lines |
|-----------|----------|-------------|------------|
| `ShadowSession` | `cypher-client-core/src/onion/shadow.rs` | Ephemeral identity + TLS connection (direct or via Tor) | ~150 |
| `TorTransport` | `cypher-client-core/src/onion/tor.rs` | arti integration: bootstrap, connect, circuit management | ~200 |
| `CircuitBuilder` | `cypher-client-core/src/onion/circuit.rs` | 0-RTT ephemeral key exchange with relay (Tier 2) | ~150 |
| `TransportPool` | `cypher-client-core/src/onion/pool.rs` | Manages 3 parallel Tor sessions / relay circuits, fallback chain, pre-warming | ~300 |
| `OnionEncoder` | `cypher-client-core/src/onion/encoder.rs` | Build 1-layer onion for relay (Tier 2) | ~100 |
| `OnionDecoder` | `cypher-client-core/src/onion/decoder.rs` | Peel onion layer, verify response integrity | ~80 |
| `PipelinedFetcher` | `cypher-client-core/src/onion/fetcher.rs` | Distribute inbox_ids, pipeline requests + ACKs, merge responses | ~250 |
| `JitterScheduler` | `cypher-client-core/src/onion/jitter.rs` | Random exponential jitter + long pause insertion | ~50 |
| `RelayHandler` | `cypher-client-core/src/onion/relay.rs` | Process relay_request on Server 2 relay side | ~150 |
| `AnonymityIndicator` | `cypher-client-core/src/onion/indicator.rs` | Compute & emit anonymity level (0-3) | ~60 |
| `CoverTraffic` | `cypher-client-core/src/onion/cover.rs` | Background dummy fetches + chaff, power-aware modes | ~150 |
| `PaddingCodec` | `cypher-client-core/src/onion/padding.rs` | Pad/unpad to fixed bucket sizes | ~50 |
| `ServerSigner` | `services/signaling/src/signing.rs` | Sign inbox responses + two-phase claim/ACK | ~120 |
| `cypher-relay` | `services/relay/src/main.rs` | Relay binary for Server 2 | ~200 |

### Modified

| Component | Change |
|-----------|--------|
| `dispatch_inbound` ([api.rs:833](crates/cypher-client-core/src/api.rs#L833)) | Detect `0x02` prefix → route to `RelayHandler` |
| `ChatSend` encryption ([api.rs:512](crates/cypher-client-core/src/api.rs#L512)) | Apply padding before encryption |
| `InboxFetchBatch` ([api.rs:260](crates/cypher-client-core/src/api.rs#L260)) | Replace with `PipelinedFetcher` → transport pool |
| Signaling `inbox_fetch` handler | Two-phase: claim + ACK; add server_sig |
| `core.p2p` proto schema | Add `inbox.ack` (`@0xC2000005`) |
| Gateway `route_frame` | No changes (onion traffic inside ChatSend) |
| UI StatusBar | Display `AnonymityIndicator` level (0-3) with shield icon |
| `Cargo.toml` (client-core) | Add `arti-client`, `arti-hyper` dependencies |

---

## Data Flow: Complete Onion Fetch

### Tier 1 (Tor) — Typical Flow

```
Alice has 20 contacts. Tor is bootstrapped. 3 Tor shadow sessions ready.

1. TransportPool has 3 Tor shadow sessions (different exit IPs):
   Shadow_A: Alice → Tor circuit 1 (exit: 198.51.100.42) → Gateway
   Shadow_B: Alice → Tor circuit 2 (exit: 203.0.113.17)  → Gateway
   Shadow_C: Alice → Tor circuit 3 (exit: 192.0.2.88)    → Gateway

2. Distribute 20 inbox_ids + 7 dummies = 27 total, 9 per transport:
   Shadow_A: [inbox_3, DUMMY, inbox_11, inbox_7, DUMMY, inbox_19, inbox_1, inbox_15, inbox_9]
   Shadow_B: [DUMMY, inbox_4, inbox_12, inbox_8, inbox_20, inbox_2, DUMMY, inbox_16, inbox_10]
   Shadow_C: [inbox_5, DUMMY, inbox_13, inbox_17, inbox_6, inbox_14, DUMMY, inbox_18, DUMMY]

3. All 3 pipeline in parallel with random exponential jitter:
   T=0ms     Shadow_A: InboxFetch(inbox_3)
             Shadow_B: InboxFetch(DUMMY)
             Shadow_C: InboxFetch(inbox_5)
   T=73ms    Shadow_A: InboxFetch(DUMMY)
   T=112ms   Shadow_B: InboxFetch(inbox_4)
   T=502ms   Shadow_A: [long pause]
   ...
   T=~1200ms All 27 requests sent (9 per session × 3 sessions)
   T=~1800ms Last response arrives (~1200ms + ~600ms Tor RTT)

4. For each response: verify server_sig, send ACK, decrypt messages

5. Server sees:
   - 3 different Tor exit IPs, 3 different ephemeral peer_ids
   - Each fetched 9 inboxes — server cannot correlate the 3 sessions
   - Cannot link any of them to Alice

Total: ~1.8s for 20 contacts via Tor (3 parallel sessions)
```

### Tier 2 (Relay) — Typical Flow

```
Same scenario, but Tor is unavailable. 3 relay circuits to Server 2 ready.

1. TransportPool has 3 relay circuits (3 independent shadow sessions):
   Shadow_A → Relay(Server 2), circuit_key_A established
   Shadow_B → Relay(Server 2), circuit_key_B established
   Shadow_C → Relay(Server 2), circuit_key_C established

2. Distribute 20 + 7 dummies = 27 total, 9 per circuit:
   (shuffled within each — relay sees random order, different peer_ids)

3. All 3 circuits fire in parallel with random exponential jitter:
   T=0ms     Shadow_A → Relay: InboxFetch(inbox_3)
             Shadow_B → Relay: InboxFetch(DUMMY)
             Shadow_C → Relay: InboxFetch(inbox_5)
   T=67ms    Shadow_A: InboxFetch(DUMMY)
   ...
   T=~1200ms All 27 requests sent
   T=~1270ms Last response arrives (~1200ms + ~70ms relay RTT)

4. Relay sees: 3 unrelated peers (different ephemeral peer_ids) asked for
   9 inboxes each. Cannot correlate them to same user.
   Server sees: Relay fetched 27 inboxes. Doesn't know for whom.

Total: ~1.3s for 20 contacts via relay (fast, trust-the-operator)
```

---

## Implementation Phases

### Phase A: Core Transport + Relay (~1100 lines)

1. `PaddingCodec` — pad/unpad all ChatSend payloads
2. `ShadowSession` — ephemeral identity + TLS connection
3. `CircuitBuilder` — 0-RTT key exchange with relay
4. `OnionEncoder` / `OnionDecoder` — 1-layer onion for relay
5. `RelayHandler` — process relay_request on relay side
6. `PipelinedFetcher` + `JitterScheduler` — parallel pipeline with anti-fingerprint jitter
7. `TransportPool` — manages relay circuit, pre-warming, fallback to direct
8. `AnonymityIndicator` — Level 0/2 display
9. Two-phase fetch — `inbox.ack` proto, signaling claim/ACK
10. `ServerSigner` — sign inbox responses
11. `cypher-relay` binary — deploy on Server 2
12. `CoverTraffic` — dummy fetches + chaff, power-aware modes

**Deliverable:** Anonymous inbox fetch via Server 2 relay. Level 2 anonymity.
Works immediately with your 2 servers.

### Phase B: Tor Integration (~500 lines)

1. `TorTransport` — arti bootstrap, 3 parallel circuit management
2. Tor bridges — obfs4/snowflake/meek support, built-in + custom bridge config
3. `TransportPool` update — Tor as preferred transport, fallback chain
4. `AnonymityIndicator` update — Level 3 for Tor
5. Tor bootstrap on app launch (background, non-blocking)
6. Settings UI — bridge configuration, speed/anonymity slider
7. Platform-specific: arti (desktop), Orbot (Android), fallback (iOS)

**Deliverable:** Level 3 anonymity via Tor. Works even in censored countries
via bridges. Zero additional infrastructure cost.

### Phase C: Future — DHT + Organic Relays (~800 lines)

Only needed when user base grows to 100+ and organic relays make sense.

1. `cypher-dht` crate — Kademlia overlay for relay discovery
2. Organic relay opt-in (desktop clients as relays)
3. Multi-relay selection with anti-collusion (2-hop when 2+ relays available)
4. Relay reputation scoring

**Deliverable:** Decentralized relay pool, reduced infra dependence.

---

## Estimated Total

| Phase | Lines | Depends on | When |
|-------|-------|------------|------|
| Phase A: Core + Relay | ~1150 | Nothing | **Now** |
| Phase B: Tor + Bridges | ~500 | Phase A | **Now** (parallel) |
| Phase C: DHT + Organic | ~800 | Phase A | At 100+ users |
| **Total** | **~2450** | | |

**Phase A + B can be developed in parallel.** Phase A gives working anonymity
via relay immediately. Phase B upgrades to Tor-level anonymity.

---

## Appendix: Latency Model (p50 / p95 / p99)

### Variables

| Symbol | Meaning | p50 | p95 | p99 |
|--------|---------|-----|-----|-----|
| **a** | Alice ↔ Gateway RTT | 20ms | 60ms | 120ms |
| **r** | Relay (Server 2) ↔ Gateway RTT | 15ms | 40ms | 80ms |
| **n** | NATS routing overhead | 2ms | 5ms | 10ms |
| **s** | Server processing (Redis + signing) | 5ms | 12ms | 25ms |
| **t** | Tor circuit RTT (3 hops) | 400ms | 800ms | 1500ms |

### Single InboxFetch RTT

**Tier 1 (Tor):** `RTT = t + s`

| | Result |
|-|--------|
| **p50** | **405ms** |
| **p95** | **812ms** |
| **p99** | **1525ms** |

**Tier 2 (Relay):** `RTT = a + 2r + 3n + 2s`

| | Formula | Result |
|-|---------|--------|
| **p50** | 20 + 30 + 6 + 10 | **66ms** |
| **p95** | 60 + 80 + 15 + 24 | **179ms** |
| **p99** | 120 + 160 + 30 + 50 | **360ms** |

**Tier 3 (Direct):** `RTT = a + n + s`

| | Result |
|-|--------|
| **p50** | **27ms** |
| **p95** | **77ms** |
| **p99** | **155ms** |

### Full 20-Contact Pipelined Fetch (3 Parallel Transports)

```
27 requests (20 real + 7 dummy) split across 3 parallel transports.
Per transport: 9 requests, random exponential jitter (mean 100ms) + 1 long pause.
Pipeline per transport ≈ 8 × 100ms + 1 × 650ms ≈ 1450ms
Total = max(transport_A, transport_B, transport_C) pipeline + last RTT
```

**Tier 1 (Tor), 3 parallel Tor sessions, balanced mode:**

| | Pipeline (per transport) | + Last RTT | max of 3 | **Total** |
|-|--------------------------|-----------|----------|-----------|
| **p50** | ~1100ms | + 405ms = 1505ms | × 1.08 | **~1.6s** |
| **p95** | ~1500ms | + 812ms = 2312ms | × 1.15 | **~2.7s** |
| **p99** | ~1800ms | + 1525ms = 3325ms | × 1.20 | **~4.0s** |

**Tier 2 (Relay), 3 parallel circuits, balanced mode:**

| | Pipeline (per transport) | + Last RTT | max of 3 | **Total** |
|-|--------------------------|-----------|----------|-----------|
| **p50** | ~1100ms | + 66ms = 1166ms | × 1.08 | **~1.3s** |
| **p95** | ~1500ms | + 179ms = 1679ms | × 1.15 | **~1.9s** |
| **p99** | ~1800ms | + 360ms = 2160ms | × 1.20 | **~2.6s** |

**Tier 2 "Max speed" (no pauses, jitter 20-200ms, 3 circuits):**

| | Pipeline | + Last RTT | max of 3 | **Total** |
|-|----------|-----------|----------|-----------|
| **p50** | ~550ms | + 66ms = 616ms | × 1.08 | **~670ms** |
| **p95** | ~900ms | + 179ms = 1079ms | × 1.15 | **~1.2s** |
| **p99** | ~1100ms | + 360ms = 1460ms | × 1.20 | **~1.8s** |

### Single Inbox "Pull to Refresh"

| Tier | p50 | p95 | p99 |
|------|-----|-----|-----|
| Tor | 405ms | 812ms | 1525ms |
| Relay | **66ms** | 179ms | 360ms |
| Direct | 27ms | 77ms | 155ms |

### Cold Start (First Fetch After App Launch)

| | Transport setup | + Fetch | Total |
|-|-----------------|---------|-------|
| **Relay p50** | ~200ms (3 circuits) | + 1.3s | **~1.5s** |
| **Tor p50** | 3s (cached consensus) | + 1.6s | **~4.6s** |

In practice: first fetch uses **relay** (3 circuits ready in ~200ms).
Tor bootstraps in background, subsequent fetches switch to Tor.

### Comparison Table

| Scenario | p50 | p95 | p99 |
|----------|-----|-----|-----|
| Direct (current, no onion) | ~27ms | ~77ms | ~155ms |
| Single inbox, Relay | 66ms | 179ms | 360ms |
| Single inbox, Tor | 405ms | 812ms | 1525ms |
| 20 contacts, Relay (balanced) | **1.3s** | 1.9s | 2.6s |
| 20 contacts, Relay (max speed) | **670ms** | 1.2s | 1.8s |
| 20 contacts, Tor (balanced) | **1.6s** | 2.7s | 4.0s |
| 20 contacts, Cold start (relay) | **1.5s** | 2.1s | 2.8s |

---

## Open Questions

### Resolved

1. ~~**Нужно 3 сервера**~~ → 2 сервера достаточно. Tier 2 (relay) на Server 2,
   Tier 1 (Tor) не требует инфраструктуры.

2. ~~**Relay infra cost**~~ → Server 2 уже есть. Tor бесплатен.

3. ~~**Guard+Exit collusion**~~ → Tier 1 (Tor) полностью решает: 3 независимых
   hop'а от волонтёров. Tier 2 — trust-the-operator (honest about it).

4. ~~**Small network DHT**~~ → Отложено до Phase C. На старте Tor + 1 relay.

### Remaining

1. **arti binary size:** Embedding arti adds ~5MB to the binary. Acceptable
   for desktop, may need feature-flag for mobile.

2. **Tor on iOS:** Apple restricts background network proxies. iOS users
   limited to Tier 2 (relay) or Tier 3 (direct).

3. ~~**Tor blocked countries**~~ → Tor bridges (obfs4, snowflake, meek) bypass
   DPI censorship. Built-in bridges shipped with app + user can add custom
   bridges. Fallback chain: direct Tor → bridges → relay → direct.

4. **Relay incentives (Phase C):** When organic relays become relevant,
   need incentive mechanism (reciprocity, reputation boost).

5. **Legal:** Relay on Server 2 sees encrypted blobs only. No-logging by design
   (in-memory state only). Tor usage is legal in most jurisdictions.
