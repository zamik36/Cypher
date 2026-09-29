# Commands Reference

## Prerequisites

| Tool | Install |
|------|---------|
| Rust | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |
| Just | `cargo install just` |
| Docker | [docker.com](https://docs.docker.com/get-docker/) |
| Node.js | Required for desktop/PWA clients |

---

## Just (task runner)

### Infrastructure

```bash
just infra              # Start Redis + NATS (docker compose up -d)
just infra-down         # Stop Redis + NATS
```

### Build

```bash
just build              # cargo build --workspace
just build-release      # cargo build --workspace --release
```

### Backend Services

```bash
just gateway            # Run gateway (TLS :9100, WS :9101, metrics :9090)
just signaling          # Run signaling (NATS subscriber, metrics :9091)
just relay              # Run relay (TLS :9300, metrics :9092)
just services           # Run all 3 services in parallel
```

### Tests & Linting

```bash
just test               # cargo test --workspace (45 tests)
just lint               # cargo clippy --workspace -- -D warnings
just check              # test + lint
```

### Desktop App (Tauri)

```bash
just desktop-deps       # npm install (frontend deps)
just desktop-dev        # Run desktop app with hot-reload
just desktop-build      # Build release binary (.exe / .dmg / .AppImage)
```

### Android

```bash
just android-dev        # Run on connected device/emulator
just android-debug      # Build debug APK
just android-release    # Build release APK (unsigned)
just android-sign       # Build + sign release APK
```

### PWA

```bash
just pwa-deps           # npm install
just pwa-dev            # Dev server on http://0.0.0.0:5174
just pwa-build          # Production build to dist/
just pwa-serve          # Serve built PWA on LAN
```

### Full Stack

```bash
just test-local         # Start everything: infra + services + PWA dev server
```

---

## Service Endpoints

| Service | Address | Metrics |
|---------|---------|---------|
| Gateway (TLS) | `0.0.0.0:9100` | `:9090/metrics` |
| Gateway (WS) | `0.0.0.0:9101` | — |
| Signaling | via NATS | `:9091/metrics` |
| Relay (TLS) | `0.0.0.0:9300` | `:9092/metrics` |
| Redis | `localhost:6379` | — |
| NATS | `localhost:4222` | `:8222` |
| PWA | `http://0.0.0.0:5174` | — |

---

## Typical Workflows

### First time setup
```bash
rustup show             # installs the pinned toolchain; cargo install just
just infra              # start Redis + NATS
just deps               # install frontend deps
```

### Daily development
```bash
just infra              # ensure infra is running
just services &         # start backend
just desktop-dev        # run desktop app with hot-reload
```

### Before commit
```bash
just check              # lint + hygiene + tests
```

### Build release
```bash
just build-release      # Rust binaries
just desktop-build      # Desktop installer
just android-sign       # Signed Android APK
```
