# Commands Reference

## Prerequisites

| Tool | Install |
|------|---------|
| Rust | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |
| Just | `cargo install just` |
| Docker | [docker.com](https://docs.docker.com/get-docker/) |
| Nix | `curl --proto '=https' --tlsv1.2 -sSf -L https://install.determinate.systems/nix \| sh` |
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

## Nix Flakes

### Dev Shell

```bash
nix develop                         # Enter dev shell with all tools
nix develop --command bash          # Enter with bash instead of default shell
direnv allow                        # Auto-activate shell on cd (requires direnv)
```

The dev shell provides: Rust stable + clippy/rustfmt, cargo-nextest, cargo-watch,
cargo-fuzz, cargo-deny, cargo-audit, cargo-machete, redis-cli, natscli, just, jq,
hyperfine.

### Build Services

```bash
nix build .#gateway                 # Build gateway binary -> ./result/bin/gateway
nix build .#signaling               # Build signaling binary
nix build .#relay                   # Build relay binary
nix build .#load-test               # Build load-test tool
nix build                           # Build all 3 services (default package)
```

### Run Services

```bash
nix run .#gateway                   # Build and run gateway
nix run .#signaling                 # Build and run signaling
nix run .#relay                     # Build and run relay
nix run .#load-test                 # Build and run load-test
```

### Docker Images (without Dockerfile)

```bash
nix build .#docker-gateway          # Build minimal OCI image (~20MB)
nix build .#docker-signaling
nix build .#docker-relay

# Load into Docker
docker load < result
docker run -it --rm gateway:latest
```

### CI Checks

```bash
nix flake check                     # Run ALL checks in parallel:
                                    #   - clippy (warnings = errors)
                                    #   - rustfmt (formatting)
                                    #   - cargo-nextest (tests)
                                    #   - rustdoc (doc warnings)
```

### Inspect Flake

```bash
nix flake show                      # List all outputs (packages, checks, apps)
nix flake metadata                  # Show inputs and lock info
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
nix develop             # or: rustup, cargo install just
just infra              # start Redis + NATS
just desktop-deps       # install frontend deps
```

### Daily development
```bash
just infra              # ensure infra is running
just services &         # start backend
just desktop-dev        # run desktop app with hot-reload
```

### Before commit
```bash
just check              # tests + clippy
# or with Nix:
nix flake check         # tests + clippy + fmt + docs
```

### Build release
```bash
just build-release      # Rust binaries
just desktop-build      # Desktop installer
just android-sign       # Signed Android APK
```
