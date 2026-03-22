# Шифр (Cypher) — build & run commands
# Install: cargo install just
# Usage:  just <recipe>    (run `just` without args to see all recipes)

set windows-shell := ["bash", "-cu"]

# Default: show available recipes
default:
    @just --list

# ─── Infrastructure ──────────────────────────────────────────────────────────

# Start Redis + NATS in Docker
infra:
    docker compose up -d

# Stop Redis + NATS
infra-down:
    docker compose down

# ─── Backend services ────────────────────────────────────────────────────────

# Build all Rust crates
build:
    cargo build --workspace

# Build in release mode
build-release:
    cargo build --workspace --release

# Run gateway service
gateway:
    cargo run -p gateway

# Run signaling service
signaling:
    cargo run -p signaling

# Run relay service
relay:
    cargo run -p relay

# Run all 3 backend services (gateway + signaling + relay)
services:
    #!/usr/bin/env bash
    set -e
    echo "Starting gateway, signaling, relay..."
    cargo run -p gateway &
    cargo run -p signaling &
    cargo run -p relay &
    wait

# ─── Tests ───────────────────────────────────────────────────────────────────

# Run all tests
test:
    cargo test --workspace

# Run clippy
lint:
    cargo clippy --workspace -- -D warnings

# Run tests + clippy
check: test lint

# ─── Desktop (Windows/Linux/macOS) ──────────────────────────────────────────

# Install desktop frontend dependencies
desktop-deps:
    cd clients/desktop && npm install

# Run desktop app in dev mode (hot-reload)
desktop-dev:
    cd clients/desktop && cargo tauri dev

# Build desktop app (release)
desktop-build:
    cd clients/desktop && cargo tauri build

# ─── Android ─────────────────────────────────────────────────────────────────

# Run Android app in dev mode (needs connected device/emulator)
android-dev:
    cd clients/desktop && cargo tauri android dev

# Build Android debug APK
android-debug:
    cd clients/desktop && cargo tauri android build --apk

# Build Android release APK (unsigned)
android-release:
    cd clients/desktop && cargo tauri android build --apk --release

# Build Android release APK and sign it
android-sign: android-release
    #!/usr/bin/env bash
    set -e
    export ANDROID_HOME="C:/Users/Ilya/AppData/Local/Android/Sdk"
    KEYSTORE="clients/desktop/src-tauri/gen/android/release.keystore"
    APK_DIR="clients/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release"
    APK_UNSIGNED="$APK_DIR/app-universal-release-unsigned.apk"
    APK_SIGNED="$APK_DIR/cypher-release-signed.apk"

    # Generate keystore if not exists
    if [ ! -f "$KEYSTORE" ]; then
        echo "Generating debug signing keystore..."
        keytool -genkeypair -v \
            -keystore "$KEYSTORE" \
            -keyalg RSA -keysize 2048 -validity 10000 \
            -alias cypher \
            -storepass p2ptest123 -keypass p2ptest123 \
            -dname "CN=Cypher Dev, O=Cypher, L=Dev, C=US"
        echo "Keystore created: $KEYSTORE"
    fi

    # Find the unsigned APK
    if [ ! -f "$APK_UNSIGNED" ]; then
        echo "Looking for APK..."
        APK_UNSIGNED=$(find clients/desktop/src-tauri/gen/android/app/build/outputs/apk -name "*unsigned*.apk" | head -1)
    fi

    echo "Signing $APK_UNSIGNED ..."

    # Align
    BT="$ANDROID_HOME/build-tools/$(ls "$ANDROID_HOME/build-tools" | sort -V | tail -1)"
    "$BT/zipalign.exe" -f 4 "$APK_UNSIGNED" "$APK_SIGNED"

    # Sign with apksigner
    "$BT/apksigner.bat" sign \
        --ks "$KEYSTORE" \
        --ks-key-alias cypher \
        --ks-pass pass:p2ptest123 \
        --key-pass pass:p2ptest123 \
        "$APK_SIGNED"

    echo ""
    echo "=== Signed APK ready ==="
    realpath "$APK_SIGNED"
    echo ""

# ─── iOS / PWA ───────────────────────────────────────────────────────────────

# Install PWA dependencies
pwa-deps:
    cd clients/pwa && npm install

# Run PWA dev server (accessible on LAN at http://<your-ip>:5174)
pwa-dev:
    cd clients/pwa && npm run dev

# Build PWA for production
pwa-build:
    cd clients/pwa && npm run build

# Serve built PWA on LAN (for iOS testing via Add to Home Screen)
pwa-serve port="5174":
    cd clients/pwa && npx serve dist -l {{port}} --no-clipboard

# ─── Full stack (for testing) ────────────────────────────────────────────────

# Start everything for local WiFi testing: infra + services + PWA dev server
test-local:
    #!/usr/bin/env bash
    set -e
    echo "=== Starting infrastructure ==="
    docker compose up -d
    sleep 2

    echo ""
    echo "=== Starting backend services ==="
    cargo run -p gateway &
    cargo run -p signaling &
    cargo run -p relay &
    sleep 3

    echo ""
    echo "=== Starting PWA dev server ==="
    cd clients/pwa && npm run dev &

    echo ""
    echo "========================================="
    echo "  All services running!"
    echo ""
    echo "  Gateway TLS:  0.0.0.0:9100"
    echo "  Gateway WS:   0.0.0.0:9101"
    echo "  Signaling:    0.0.0.0:9200"
    echo "  Relay:        0.0.0.0:9300"
    echo "  PWA:          http://0.0.0.0:5174"
    echo ""
    echo "  From phone, open: http://$(hostname -I 2>/dev/null | awk '{print $1}' || echo '<your-ip>'):5174"
    echo "========================================="
    wait
