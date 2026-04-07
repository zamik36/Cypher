# Roadmap — Шифр (Cypher)

> Статус: Phase 0–8 в основном завершены. Ниже — план дальнейшего развития.

---

## Текущее состояние (реализовано)

| Phase | Что сделано |
|-------|------------|
| **0 · Foundation** | Cargo workspace, p2p-common, docker-compose (Redis + NATS), CI-ready |
| **1 · Protocol** | p2p-proto IDL + codegen (nom), wire format, p2p-crypto (X3DH + Double Ratchet + AES-256-GCM), p2p-transport (frame codec + session) |
| **2 · Gateway + Signaling** | Gateway TLS connection manager (DashMap + NATS), Signaling (Redis prekeys/links/ICE), `peer_joined` уведомления |
| **3 · NAT Traversal** | p2p-nat: STUN client, IceAgent, HolePuncher (UDP hole punching), Relay service + RelayClient |
| **4 · File Transfer** | p2p-transfer (FileChunker + FileAssembler), ClientApi send_file/accept_file, gateway direct routing для file chunks, signaling generic file forwarding |
| **5 · Desktop Client** | Tauri 2.0 + SolidJS: все команды подключены к реальному ClientApi, event loop, drag-and-drop отправка, auto-accept входящих файлов |
| **6 · Hardening** | E2EE шифрование файловых чанков (Double Ratchet), TLS на gateway и relay, fuzz тесты (5 targets), rate limiting (token bucket), SHA-256 проверка целостности файлов |
| **7 · NAT (частично)** | STUN server (IPv4+IPv6) в signaling, DTLS session (`p2p-nat/src/dtls.rs`), Relay client (`p2p-nat/src/relay_client.rs`), ICE candidate exchange через signaling |
| **8 · File Transfer улучшения** | Windowed flow control (sliding window + ack), resume после обрыва (save_state/load_state + FileResume + signaling routing), сжатие zstd (auto-detection, threshold 10%) |
| **9 · Метрики** | Prometheus метрики на всех 3 сервисах (:9090, :9091, :9092) |
| **10 · Mobile** | Android: Tauri 2.0 mobile (Kotlin/JNI, 4 ABI), iOS: PWA (SolidJS + WebSocket gateway на :9101) — Add to Home Screen, offline-capable |
| **11 · Desktop UX** | Тёмная тема (dark/light + prefers-color-scheme), file browser dialog, notifications, QR-коды |
| **12 · Identity & Persistence** | BIP39 persistent identity, encrypted SQLite/IndexedDB storage, JSX→TSX migration, security hardening (36 fixes) |
| **13 · Anonymous Inbox Transport** | Relay bootstrap via signaling, typed anonymous transport config, persisted relay/signaling key stores, signed inbox responses, two-phase fetch recovery, modularized client/signaling architecture |

**Тесты:** 63 теста, 0 failures. `cargo clippy --workspace` — 0 warnings.
**Fuzz targets:** 5 (proto dispatch/decode, crypto aead/ratchet, nat stun) — CI runs 60s each.
**CI:** cargo-deny audit, clippy, test, fuzz.

---

## Что реализовано в деталях (Phase 6–8)

### 6.1 E2EE шифрование файловых чанков ✅

- `send_chunks`: шифрование каждого чанка через `keys.encrypt_for_peer()` (Double Ratchet)
- `dispatch_inbound FileChunk`: расшифровка через `keys.decrypt_from_peer()`
- FileChunk proto содержит `ratchet_key` и `msg_no` для DH ratchet
- Файл: `crates/p2p-client-core/src/api.rs`

### 6.2 TLS на всех сервисах ✅

- Gateway: TLS через `TransportListener` (tokio-rustls)
- Relay: TLS через `TlsAcceptor`
- Клиент: `connect_to_gateway()` использует TLS с insecure verifier (dev), `connect_to_gateway_with_config()` для production

### 6.3 Fuzz тесты ✅

- `crates/p2p-proto/fuzz/` — dispatch, decode_bytes
- `crates/p2p-crypto/fuzz/` — aead, ratchet
- `crates/p2p-nat/fuzz/` — stun
- CI: 60s per target

### 6.4 Rate limiting ✅

- `crates/p2p-common/src/ratelimit.rs` — Token bucket
- Интеграция в gateway и relay

### 6.5 Проверка целостности файла ✅

- `FileAssembler::verify()` — SHA-256 хэш всего файла после сборки
- Вызывается автоматически при получении последнего чанка
- При несовпадении — `ClientEvent::Error("file integrity verification failed")`

### 7.3 DTLS session ✅

- `crates/p2p-nat/src/dtls.rs` — DTLS-like secure framing поверх UDP

### 7.4 Relay client ✅

- `crates/p2p-nat/src/relay_client.rs` — TURN-like fallback

### 7.2 ICE candidate exchange ✅

- `SignalIceCandidate` обработчик в gateway/signaling
- `gather_candidates()` → отправка кандидатов через signaling
- Автоматическое добавление remote candidates в IceAgent

### 8.1 Resume после обрыва ✅

- `FileAssembler::save_state()` / `load_state()` — bitset на диск
- `FileResume` proto сообщение — peer запрашивает недостающие чанки
- `send_chunks` с `selective_indices` — пересылка только missing

### 8.2 Windowed flow control ✅

- `TransferSender::run()` с `DEFAULT_WINDOW_SIZE`
- `FileChunkAck` → ack channel → sliding window
- Backpressure: sender ждёт ack перед отправкой следующих чанков

### 8.4 Сжатие (zstd) ✅

- Auto-detection: trial-compress первый чанк, threshold >10% savings
- `compress_chunk()` / `decompress_chunk()` в p2p-transfer
- Zstd level 3, per-file решение

### 9.4 Prometheus метрики ✅

- `p2p_common::metrics::spawn_metrics_server(port)`
- Gateway :9090, Signaling :9091, Relay :9092

### 11.1 Desktop UX ✅

- Тёмная/светлая тема с `prefers-color-scheme`
- File browser dialog
- Нативные notifications
- QR-коды для share links

---

## Что НЕ сделано — дальнейшие шаги

### Приоритет 1 (Высокий) — NAT Traversal completion

~~**7.1 STUN server в signaling**~~ ✅

- RFC 5389 UDP server в `services/signaling/src/main.rs` (StunServer struct)
- IPv4 + IPv6 XOR-MAPPED-ADDRESS
- 3 теста: IPv4 encoding, IPv6 encoding, binding roundtrip через StunClient

**7.5 E2E тест через реальный NAT**

```bash
# Два Docker контейнера в разных network namespaces с NAT:
docker run --network net_a p2p-peer-a
docker run --network net_b p2p-peer-b
# Ожидаем: P2P соединение, передача файла 10MB, verify SHA-256
```

### Приоритет 2 (Средний) — File Transfer polish

**8.3 Параллельная передача нескольких файлов**

Уже работает структурно (DashMap по file_id), но нужно:
- Приоритизацию: маленькие файлы не должны блокироваться большими
- UI для выбора порядка передачи

### Приоритет 3 (Низкий) — Масштабирование

**9.1 Gateway clustering**

- Добавить `gateway_node_id` в Redis `peer:{id}:session`
- L4 load balancer: `SO_REUSEPORT` для распределения новых TCP соединений

**9.2 Signaling clustering**

- NATS JetStream для reliable messaging между signaling узлами
- Redis Cluster для шардирования по peer_id hash

**9.3 Relay geographic distribution**

- Несколько регионов: EU, US-East, AP
- Клиент выбирает ближайший relay по latency (STUN ping)

**9.5 Нагрузочное тестирование**

```bash
# Цель: 10K → 100K concurrent connections на gateway
cargo run --bin load-test -- --connections 10000 --duration 60s
# Проверить: память < 2GB на 100K connections, p99 latency < 50ms
```

---

## Phase 10 — Мобильные клиенты ✅

> **Подход:** Tauri 2.0 mobile для Android, PWA для iOS (не нужна подписка Apple Developer).

### 10.1 Android (Tauri 2.0 Mobile) ✅

- `clients/desktop/src-tauri/gen/android/` — полный Kotlin + Gradle проект
- Package: `dev.p2p.app`, minSdk 24, targetSdk 36
- 4 ABI: arm64-v8a, armeabi-v7a, x86, x86_64
- JNI: `System.loadLibrary("desktop_lib")` → тот же Rust backend
- Один SolidJS фронтенд для desktop и mobile

### 10.2 iOS PWA ✅

- `clients/pwa/` — standalone SolidJS + Vite PWA
- WebSocket подключение к gateway (порт :9101, настраивается через `P2P_WS_ADDR`)
- Бинарный протокол (TypeScript codec, `src/api/proto.ts`) идентичен Rust wire format
- Offline-capable: Service Worker + cache-first стратегия
- Установка: Safari → "Add to Home Screen" — standalone app без адресной строки
- `manifest.json` + `apple-mobile-web-app-capable` мета-теги
- Тот же UI: dark/light тема, responsive design (<640px mobile layout)
- Файлы: drag-drop / file picker, chunking (64KB), SHA-256, авто-скачивание
- Chat: PWA ↔ PWA (plaintext); E2EE для PWA ↔ Desktop — future (WASM crypto)

### 10.3 Gateway WebSocket endpoint ✅

- `services/gateway/src/main.rs` — `ws_accept_loop()` рядом с TLS accept loop
- Binary WS messages = proto payloads (без дополнительного framing)
- SESSION_INIT определяется по constructor ID (0xA1000001)
- Тот же routing через NATS, те же ConnectionState/DashMap/heartbeat
- Config: `ws_addr` (default `0.0.0.0:9101`, env `P2P_WS_ADDR`)

---

## Phase 11 — UX и polish (оставшееся)

### 11.2 Шифрование локального хранилища ✅

- Desktop: SQLite + AES-256-GCM + zstd compression (adaptive — skip для payload < 1KB)
  - `crates/cypher-client-core/src/persistence/sqlite.rs` — encrypted message store
  - `crates/cypher-client-core/src/identity_store.rs` — Argon2id + AES-256-GCM для seed
- PWA: IndexedDB + Web Crypto API (PBKDF2 600K iterations + AES-256-GCM)
  - `clients/pwa/src/storage/messages.ts` — encrypted message history
  - `clients/pwa/src/storage/identity.ts` — persistent identity в localStorage
  - `clients/pwa/src/storage/crypto.ts` — PBKDF2/HKDF/AES-GCM helpers
- Хранятся: сообщения, ratchet-состояния, conversations metadata
- Автоматическое восстановление ratchet state при unlock identity

### 11.3 Голосовые/видео звонки

- WebRTC (через `webrtc-rs`) поверх установленного P2P канала
- Используем уже существующий ICE/DTLS pipeline
- Видео: VP8/H.264 через `webrtc-rs` data channels

---

---

## Phase 12 — Identity & Security Hardening ✅

### 12.1 Persistent Identity (BIP39) ✅
- `cypher-crypto/src/identity.rs` — IdentitySeed с BIP39 мнемоникой (24 слова)
- HKDF деривация: Ed25519 (signing), X25519 (DH), Storage Encryption Key
- Identity store: Argon2id (desktop) / PBKDF2 (PWA) + AES-256-GCM encryption
- Export/import seed через hex или мнемонику

### 12.2 JSX → TypeScript (strict mode) ✅
- Полная миграция обоих клиентов: `.jsx` → `.tsx`
- TypeScript strict mode, typed event emitter, proper interfaces
- Удалены все `.jsx` / `.js` файлы (28 файлов)

### 12.3 Security Hardening (36 fixes) ✅
- Nonce derivation: message_key включён в nonce material (defense-in-depth)
- X3DH mutual: SPK включён в DH exchange (3 DH-значения, sorted cross-terms)
- CSP meta tags на всех клиентах
- Seed auto-clear (30s), hex validation, passphrase min 12 chars
- globalThis → module-scoped variable
- File hash verification (SHA-256) на стороне получателя
- WebSocket resolve только после SessionAck
- Global skipped keys limit (2048), panic→graceful error
- Network-first SW cache, file transfer timeout (5 min)
- HashSet для peers (O(1)), typed EventMap, message size limit (50KB)
- Sqlite mutex: Result вместо expect (no panic in production)
- Desktop IdentityView + unlock flow (feature parity с PWA)

---

## Технический долг

| Приоритет | Задача | Статус |
|-----------|--------|--------|
| ~~🔴 Высокий~~ | ~~E2EE шифрование файловых чанков~~ | ✅ Реализовано |
| ~~🔴 Высокий~~ | ~~TLS на gateway~~ | ✅ Реализовано |
| ~~🟡 Средний~~ | ~~Windowed flow control для file chunks~~ | ✅ Реализовано |
| ~~🟡 Средний~~ | ~~Resume после обрыва~~ | ✅ Реализовано |
| ~~🟡 Средний~~ | ~~STUN server в signaling~~ | ✅ IPv4+IPv6, 3 теста |
| ~~🟢 Низкий~~ | ~~Fuzz тесты для proto/crypto~~ | ✅ 5 targets |
| ~~🟢 Низкий~~ | ~~Prometheus метрики~~ | ✅ 3 сервиса |
| ~~🟢 Низкий~~ | ~~Cargo-deny audit в CI~~ | ✅ В ci.yml |

---

## Матрица зависимостей фаз

```
Phase 6 (Hardening)     ✅ завершена
Phase 7 (NAT E2E)       ⚠️ частично (STUN server ✅, нет E2E NAT test)
Phase 8 (File улучш.)   ✅ завершена
Phase 9 (Scaling)       ⚠️ только метрики; clustering не начат
Phase 10 (Mobile)       ✅ Android (Tauri 2.0) + iOS (PWA)
Phase 11 (UX)           ⚠️ desktop UX готов; storage ✅; voice не начат
Phase 12 (Identity)     ✅ завершена (persistent identity, encrypted storage, 36 security fixes)
```

---

## Метрики успеха

| Метрика | Текущее | Цель Phase 9 |
|---------|---------|-------------|
| Concurrent connections | ~100 (dev) | 100 000 |
| P2P success rate | частичный ICE | >80% |
| File transfer throughput | ~50 MB/s (relay) | >200 MB/s (direct P2P) |
| Cold start (время до "ready") | ~2s | <500ms |
| Test coverage | 63 tests + 5 fuzz | >200 tests + fuzzing |
| Memory per gateway connection | — | <10 KB |
