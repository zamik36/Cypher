# Шифр (Cypher)

Анонимный P2P-обменник файлов и сообщений. Без регистрации, без лимитов на размер файлов, с полным сквозным шифрованием.

Сервер никогда не видит plaintext — только зашифрованные байты. Нет аккаунтов, нет базы данных с пользователями. Всё, что хранится на сервере — эфемерные ключи в Redis с автоматическим TTL.

---

## Как это работает

```
Peer A                       Серверный кластер                      Peer B
┌──────┐                  ┌──────────────────┐                  ┌──────┐
│Client│──TLS/TCP─────────│    Gateway        │─────────TLS/TCP──│Client│
│      │                  │  (conn manager)   │                  │      │
│      │                  ├──────────────────┤                  │      │
│      │                  │  Signaling        │                  │      │
│      │                  │  (peer discovery) │                  │      │
│      │                  ├──────────────────┤                  │      │
│      │                  │  Relay            │                  │      │
│      │                  │  (TURN fallback)  │                  │      │
│      │                  └──────────────────┘                  │      │
│      │                                                         │      │
│      │◄══════ Direct P2P · UDP hole punch · DTLS ════════════►│      │
│      │◄══════ или Relay (если оба за Symmetric NAT) ══════════►│      │
└──────┘                                                         └──────┘
```

1. Peer A создаёт комнату, получает код
2. Peer B вводит код, подключается к тому же Gateway
3. Через Signaling происходит обмен ICE-кандидатами и X3DH prekeys
4. Устанавливается прямое P2P-соединение (UDP hole punching + DTLS)
5. Если NAT не позволяет — трафик идёт через Relay
6. Файлы и сообщения шифруются Double Ratchet (каждое сообщение — уникальный ключ)

## Возможности

- **E2EE**: X3DH key agreement + Double Ratchet + AES-256-GCM
- **P2P**: STUN/ICE, UDP hole punching, DTLS-like secure framing
- **Relay fallback**: TURN-подобный форвардинг с ограничением bandwidth
- **Chunked transfer**: файлы любого размера, sliding window + ACK, resume после обрыва
- **Сжатие**: zstd level 3, автоопределение (trial-compress, порог 10%)
- **SHA-256 integrity**: проверка целостности каждого переданного файла
- **Кастомный протокол**: бинарный wire format с IDL-кодогенерацией (аналог MTProto)
- **Метрики**: Prometheus на всех сервисах, Grafana dashboards

## Структура проекта

```
crates/
  cypher-common/       — типы (PeerId, LinkId, FileId), конфиг, трейсинг, метрики, rate limiting
  cypher-proto/        — IDL-парсер (nom) + кодогенерация, бинарный wire format, Message enum
  cypher-crypto/       — Ed25519/X25519, X3DH, Double Ratchet, AES-256-GCM
  cypher-tls/          — TLS-конфигурация (rustls), self-signed для dev
  cypher-transport/    — Frame codec (tokio_util), TransportSession, TransportListener
  cypher-nat/          — STUN client, IceAgent, HolePuncher, DtlsSession, RelayClient
  cypher-transfer/     — FileChunker, FileAssembler, zstd-сжатие
  cypher-client-core/  — высокоуровневый API: подключение, сессия, P2P, передача файлов

services/
  gateway/             — TLS connection manager, WebSocket, DashMap сессий, NATS routing
  signaling/           — NATS subscriber, Redis (prekeys/links/ICE), STUN server
  relay/               — TLS TURN-like forwarder, bandwidth limiting

clients/
  desktop/             — Tauri 2.0 + SolidJS (TypeScript), тёмная тема, нативные диалоги
  pwa/                 — SolidJS + Vite, PWA (installable, offline, Service Worker)

tools/
  load-test/           — нагрузочное тестирование Gateway (clap CLI)
```

## Быстрый старт

### Docker (рекомендуемый способ)

```bash
# Клонировать и запустить
git clone <repo-url> && cd p2p
cp .env.example .env   # настроить пароли
docker compose up -d

# Проверить что сервисы поднялись
curl http://localhost:9090/metrics   # gateway
curl http://localhost:9091/metrics   # signaling
curl http://localhost:9092/metrics   # relay
```

Gateway слушает на `:9100` (native TLS) и `:9101` (WebSocket).

### Production

```bash
docker compose -f docker-compose.yml -f docker-compose.prod.yml up -d
```

Caddy автоматически получает TLS-сертификаты через Let's Encrypt. Подробнее — в [DEPLOY.md](DEPLOY.md).

### Мониторинг (опционально)

```bash
docker compose -f docker-compose.yml -f docker-compose.monitoring.yml up -d
```

Prometheus на `:9190`, Grafana на `:3000`.

### Разработка

```bash
# Зависимости
rustup install stable
cargo install cargo-deny cargo-fuzz

# Запустить инфраструктуру (Redis + NATS)
docker compose up redis nats -d

# Собрать и запустить тесты
cargo build --workspace
cargo test --workspace

# Запустить сервисы локально
cargo run -p cypher-gateway
cargo run -p cypher-signaling
cargo run -p cypher-relay

# Клиент (PWA)
cd clients/pwa && npm install && npm run dev

# Клиент (Desktop)
cd clients/desktop && npm install && npm run tauri dev
```

Полный список команд — в [COMMANDS.md](COMMANDS.md).

## Клиенты

| Платформа | Технология | Статус |
|-----------|-----------|--------|
| Desktop (Win/Linux/macOS) | Tauri 2.0 + SolidJS | Готов |
| Web (PWA) | SolidJS + Vite + Service Worker | Готов |
| Android | Tauri 2.0 mobile (Kotlin/JNI) | Готов |
| iOS | PWA (Add to Home Screen) | Готов |

## Тесты

45 тестов, 0 failures. `cargo clippy --workspace` — 0 warnings.

```
13 crypto    — X3DH, Double Ratchet, AES-GCM
10 proto     — кодогенерация, сериализация, dispatch
 7 transport — frame codec, sessions
 4 nat       — STUN, DTLS
 3 ratelimit — token bucket
 3 compress  — zstd auto-detection
 3 signaling — STUN IPv4/IPv6, roundtrip
 1 tls       — doctest
 1 doctest
```

Fuzz targets (5): proto dispatch/decode, crypto aead/ratchet, nat stun — CI прогоняет 60 секунд на каждый.

## Документация

- [ARCHITECTURE.md](ARCHITECTURE.md) — детальное описание архитектуры и реализации
- [DEPLOY.md](DEPLOY.md) — руководство по развёртыванию
- [COMMANDS.md](COMMANDS.md) — справочник команд
- [ROADMAP.md](ROADMAP.md) — план развития

## Лицензия

MIT — [LICENSE](LICENSE)
