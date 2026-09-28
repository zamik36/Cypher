# Шифр (Cypher) — build & run commands
# Install: cargo install just
# Usage:  just <recipe>    (run `just` without args to see all recipes)
#
# Local services read secrets from .env (copy .env.example): each service
# connects to NATS as its own least-privilege user (deploy/nats.conf).

set windows-shell := ["bash", "-cu"]
set dotenv-load := true

nats_url := "nats://127.0.0.1:4222"
# Development gateways and relays write their self-signed certificates here.
certs := "target/dev-certs"

# Default: show available recipes
default:
    @just --list

# ─── Infrastructure ──────────────────────────────────────────────────────────

# Start Redis + NATS in Docker
infra:
    docker compose up -d redis nats

# Stop the Docker stack
infra-down:
    docker compose down

# ─── Backend services ────────────────────────────────────────────────────────

# Build all Rust crates
build:
    cargo build --workspace

# Build in release mode
build-release:
    cargo build --workspace --release

# Run gateway service (TLS :9100, WebSocket :9101)
gateway:
    mkdir -p {{certs}}
    P2P_NATS_URL={{nats_url}} P2P_NATS_USER=gateway P2P_NATS_PASSWORD="$GATEWAY_NATS_PASSWORD" \
    P2P_WS_ADDR=127.0.0.1:9101 P2P_DEV_CERT_OUT={{certs}}/gateway.pem cargo run -p gateway

# Run signaling service (NATS + Redis only)
signaling:
    P2P_NATS_URL={{nats_url}} P2P_NATS_USER=signaling P2P_NATS_PASSWORD="$SIGNALING_NATS_PASSWORD" \
    P2P_REDIS_URL="redis://:$REDIS_PASSWORD@127.0.0.1:6379" P2P_RELAY_PUBLIC_ADDR=localhost:9300 \
    cargo run -p signaling

# Run relay service (TLS :9300, WebSocket :9301)
relay:
    mkdir -p {{certs}}
    P2P_NATS_URL={{nats_url}} P2P_NATS_USER=relay P2P_NATS_PASSWORD="$RELAY_NATS_PASSWORD" \
    P2P_WS_ADDR=127.0.0.1:9301 P2P_DEV_CERT_OUT={{certs}}/relay.pem cargo run -p relay

# Run all 3 backend services
services:
    #!/usr/bin/env bash
    set -e
    cargo build -p gateway -p signaling -p relay
    just gateway & just signaling & just relay &
    wait

# ─── Quality ─────────────────────────────────────────────────────────────────

nightly := "nightly-2026-09-20"

# Run all tests (nextest) and doctests
test:
    cargo nextest run --workspace --all-features
    cargo test --doc --workspace --all-features

# Formatting and clippy for every target the project ships
lint:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo clippy -p cypher-desktop --all-targets -- -D warnings
    cargo clippy -p cypher-wasm -p cypher-media --target wasm32-unknown-unknown -- -D warnings

# TOML formatting, typos, unused dependencies, every feature on its own
hygiene:
    taplo fmt --check
    typos
    cargo machete
    cargo hack clippy --each-feature --all-targets -p cypher-media -p cypher-client -p cypher-transport -- -D warnings
    cargo deny --all-features check

# Undefined-behaviour check; `just miri miri-full --run-ignored all` runs everything
miri profile="miri" *args:
    MIRIFLAGS=-Zmiri-strict-provenance cargo +{{nightly}} miri nextest run --profile {{profile}} \
        -p cypher-types -p cypher-wire -p cypher-crypto -p cypher-core -p cypher-media {{args}}

# Line-coverage ratchet (.config/coverage.toml); `just cov --bump` raises floors.
# Set CYPHER_TEST_REDIS / CYPHER_TEST_NATS to include the in-process stack test.
cov *args:
    cargo llvm-cov nextest --workspace --all-features --json --summary-only --output-path target/cov.json
    cargo run -q -p xtask -- coverage-gate {{args}}

# Run lint + hygiene + tests
check: lint hygiene test

# Live end-to-end journeys against locally running services (`just infra services`)
e2e:
    cat {{certs}}/gateway.pem {{certs}}/relay.pem > {{certs}}/stack.pem
    CYPHER_LIVE_GATEWAY=localhost:9100 CYPHER_LIVE_CA={{certs}}/stack.pem cargo test --release -p e2e --test live
    npm run build:wasm -w apps/pwa
    CYPHER_WS_GATEWAY=ws://127.0.0.1:9101 CYPHER_WS_RELAY=ws://127.0.0.1:9301 npm run test:live -w apps/pwa

# Gateway load test against a local gateway
load connections="1000" duration="30":
    cargo run --release -p load-test -- --connections {{connections}} --duration {{duration}} \
        --gateway-addr localhost:9100 --ca-cert {{certs}}/gateway.pem

# ─── Frontend ────────────────────────────────────────────────────────────────

# Install all frontend dependencies (npm workspaces)
deps:
    npm install

# Build the WebAssembly core for the PWA
wasm:
    npm run build:wasm -w apps/pwa

# ─── Desktop (Windows/Linux/macOS) ──────────────────────────────────────────

# Run desktop app in dev mode (hot-reload)
desktop-dev:
    cd apps/desktop && cargo tauri dev

# Build desktop app (release)
desktop-build:
    cd apps/desktop && cargo tauri build

# ─── Android ─────────────────────────────────────────────────────────────────

# Run Android app in dev mode (needs connected device/emulator)
android-dev:
    cd apps/desktop && cargo tauri android dev

# Build Android debug APK
android-debug:
    cd apps/desktop && cargo tauri android build --apk

# Build Android release APK (unsigned)
android-release:
    cd apps/desktop && cargo tauri android build --apk --release

# Sign the release APK with a local development keystore
android-sign: android-release
    #!/usr/bin/env bash
    set -e
    : "${ANDROID_HOME:?set ANDROID_HOME to the Android SDK}"
    PASS="${CYPHER_KEYSTORE_PASS:-p2ptest123}"
    KEYSTORE="apps/desktop/src-tauri/gen/android/release.keystore"
    APK_DIR="apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release"
    APK_UNSIGNED="$APK_DIR/app-universal-release-unsigned.apk"
    APK_SIGNED="$APK_DIR/cypher-release-signed.apk"
    if [ ! -f "$KEYSTORE" ]; then
        keytool -genkeypair -v -keystore "$KEYSTORE" -keyalg RSA -keysize 2048 -validity 10000 \
            -alias cypher -storepass "$PASS" -keypass "$PASS" -dname "CN=Cypher Dev, O=Cypher, C=US"
    fi
    if [ ! -f "$APK_UNSIGNED" ]; then
        APK_UNSIGNED=$(find apps/desktop/src-tauri/gen/android/app/build/outputs/apk -name "*unsigned*.apk" | head -1)
    fi
    BT="$ANDROID_HOME/build-tools/$(ls "$ANDROID_HOME/build-tools" | sort -V | tail -1)"
    "$BT/zipalign" -f 4 "$APK_UNSIGNED" "$APK_SIGNED"
    SIGNER="$BT/apksigner"; [ -f "$SIGNER.bat" ] && SIGNER="$SIGNER.bat"
    "$SIGNER" sign --ks "$KEYSTORE" --ks-key-alias cypher --ks-pass "pass:$PASS" --key-pass "pass:$PASS" "$APK_SIGNED"
    realpath "$APK_SIGNED"

# ─── PWA ─────────────────────────────────────────────────────────────────────

# Run PWA dev server; /ws and /relay are proxied to the local services
pwa-dev: wasm
    CYPHER_DEV_GATEWAY_WS=ws://127.0.0.1:9101 CYPHER_DEV_RELAY_WS=ws://127.0.0.1:9301 npm run dev -w apps/pwa

# Build PWA for production
pwa-build: wasm
    npm run build -w apps/pwa

# ─── Full stack (for testing) ────────────────────────────────────────────────

# Start infra, the three services and the PWA dev server
test-local: infra
    #!/usr/bin/env bash
    set -e
    just services &
    just pwa-dev &
    echo "Gateway TLS :9100, WS :9101 | Relay TLS :9300, WS :9301 | PWA http://localhost:5174"
    wait
