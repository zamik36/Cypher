# Commands Reference

Everything runs through [`just`](https://github.com/casey/just); `just` with no arguments lists the recipes. Local services read secrets from `.env` (copy `.env.example`).

## Prerequisites

| Tool | Install |
|------|---------|
| Rust | [rustup](https://rustup.rs); `rust-toolchain.toml` pins the version |
| Just | `cargo install just` |
| Docker | [docker.com](https://docs.docker.com/get-docker/) — Redis and NATS |
| Node.js 22 | Desktop and PWA frontends (npm workspaces) |
| wasm-bindgen | `cargo install wasm-bindgen-cli --version 0.2.129 --locked`, for the PWA |

Quality tools (`cargo-nextest`, `cargo-llvm-cov`, `cargo-deny`, `cargo-machete`, `cargo-hack`, `taplo`, `typos`) are installed the same way; CI pins their versions in `.github/workflows/ci.yml`.

## Infrastructure and services

```bash
just infra              # Redis + NATS in Docker
just infra-down         # stop them
just gateway            # TLS :9100, WebSocket :9101, metrics :9090
just signaling          # NATS + Redis, metrics :9091
just relay              # TLS :9300, WebSocket :9301, metrics :9092
just services           # all three in parallel
just build              # cargo build --workspace
just build-release      # release build
```

Development gateways and relays write self-signed certificates to `target/dev-certs`.

## Quality

```bash
just check              # lint + hygiene + test: what CI requires of Rust
just lint               # rustfmt, clippy (workspace, desktop, wasm32)
just hygiene            # taplo, typos, machete, every feature alone, cargo-deny
just test               # nextest + doctests
just miri               # undefined-behaviour check of types/wire/crypto/core/media
just cov                # line-coverage ratchet; `just cov --bump` raises floors
just web-check          # frontend: typecheck, ESLint, Prettier, vitest with coverage
```

## End to end

```bash
just e2e                # native journey against `just services`, then web-e2e
just web-e2e            # Playwright on the production PWA bundle (needs `just wasm`)
```

The in-process stack test runs inside `just test` / `just cov` when `CYPHER_TEST_REDIS` and `CYPHER_TEST_NATS` point at a Redis and a NATS that no other services use (see [CONTRIBUTING.md](../CONTRIBUTING.md)).

## Performance

```bash
just bench              # criterion benchmarks; reports in target/criterion
just load 1000 30       # native TLS load: connections, seconds
just load-ws 100 30s    # WebSocket load with k6: pairs, hold time
just flamegraph gateway 30   # CPU flamegraph on Linux (perf + inferno)
just console gateway    # service with tokio-console instrumentation
```

Methods and results: [performance.md](performance.md).

## Clients

```bash
just deps               # npm install for all frontends
just wasm               # WebAssembly core for the PWA
just pwa-dev            # PWA dev server on :5174, proxying /ws and /relay
just pwa-build          # production PWA in apps/pwa/dist
just desktop-dev        # desktop app with hot reload
just desktop-build      # desktop installer
just android-dev        # Android on a device or emulator
just android-debug      # debug APK
just android-release    # unsigned release APK
just android-sign       # signed release APK (local keystore)
just test-local         # infra + services + PWA dev server
```

## Service endpoints

| Service | Address | Metrics |
|---------|---------|---------|
| Gateway (TLS) | `:9100` | `:9090/metrics` |
| Gateway (WebSocket) | `:9101` | — |
| Signaling | via NATS | `:9091/metrics` |
| Relay (TLS / WebSocket) | `:9300` / `:9301` | `:9092/metrics` |
| Redis | `localhost:6379` | — |
| NATS | `localhost:4222` | `:8222` |
| PWA dev server | `http://localhost:5174` | — |
