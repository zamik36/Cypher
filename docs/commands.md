# Команды

Всё запускается через [`just`](https://github.com/casey/just); `just` без аргументов показывает список рецептов. Локальные сервисы берут секреты из `.env` (скопируйте `.env.example`).

## Что установить

| Инструмент | Как |
|------|---------|
| Rust | [rustup](https://rustup.rs); версию закрепляет `rust-toolchain.toml` |
| Just | `cargo install just` |
| Docker | [docker.com](https://docs.docker.com/get-docker/) — Redis и NATS |
| Node.js 22 | Фронтенды десктопа и PWA (npm workspaces) |
| wasm-bindgen | `cargo install wasm-bindgen-cli --version 0.2.129 --locked` — для PWA |

Инструменты проверок (`cargo-nextest`, `cargo-llvm-cov`, `cargo-deny`, `cargo-machete`, `cargo-hack`, `taplo`, `typos`) ставятся так же. Их версии закреплены в `.github/workflows/ci.yml`.

## Инфраструктура и сервисы

```bash
just infra              # Redis + NATS в Docker
just infra-down         # остановить их
just gateway            # TLS :9100, WebSocket :9101, метрики :9090
just signaling          # NATS + Redis, метрики :9091
just relay              # TLS :9300, WebSocket :9301, метрики :9092
just services           # все три параллельно
just build              # cargo build --workspace
just build-release      # release-сборка
```

Gateway и relay в разработке пишут самоподписанные сертификаты в `target/dev-certs`.

## Проверки

```bash
just check              # lint + hygiene + test: всё, что CI требует от Rust
just lint               # rustfmt, clippy (workspace, десктоп, wasm32)
just hygiene            # taplo, typos, machete, каждая фича отдельно, cargo-deny
just test               # nextest + doctest'ы
just miri               # поиск UB в types/wire/crypto/core/media
just cov                # храповик покрытия; `just cov --bump` поднимает пороги
just web-check          # фронтенд: tsc, ESLint, Prettier, vitest с покрытием
```

## Сквозные сценарии

```bash
just e2e                # нативный сценарий против `just services`, затем web-e2e
just web-e2e            # Playwright на продакшен-сборке PWA (нужен `just wasm`)
```

Тест стека в процессе входит в `just test` и `just cov`, если `CYPHER_TEST_REDIS` и `CYPHER_TEST_NATS` указывают на Redis и NATS, которыми не пользуются другие сервисы (см. [CONTRIBUTING.md](../CONTRIBUTING.md)).

## Производительность

```bash
just bench              # бенчмарки criterion; отчёты в target/criterion
just load 1000 30       # нагрузка по TLS: соединения, секунды
just load-ws 100 30s    # нагрузка по WebSocket через k6: пары, время удержания
just flamegraph gateway 30   # CPU-флеймграф на Linux (perf + inferno)
just console gateway    # сервис с инструментированием для tokio-console
```

Методика и результаты — в [performance.md](performance.md).

## Клиенты

```bash
just deps               # npm install для всех фронтендов
just wasm               # WebAssembly-ядро для PWA
just pwa-dev            # dev-сервер PWA на :5174, проксирует /ws и /relay
just pwa-build          # production-сборка PWA в apps/pwa/dist
just desktop-dev        # десктоп с горячей перезагрузкой
just desktop-build      # установщик десктопа
just android-dev        # Android на устройстве или эмуляторе
just android-debug      # отладочный APK
just android-release    # release APK без подписи
just android-sign       # подписанный release APK (локальный keystore)
just test-local         # инфраструктура + сервисы + dev-сервер PWA
```

## Адреса сервисов

| Сервис | Адрес | Метрики |
|---------|---------|---------|
| Gateway (TLS) | `:9100` | `:9090/metrics` |
| Gateway (WebSocket) | `:9101` | — |
| Signaling | через NATS | `:9091/metrics` |
| Relay (TLS / WebSocket) | `:9300` / `:9301` | `:9092/metrics` |
| Redis | `localhost:6379` | — |
| NATS | `localhost:4222` | `:8222` |
| Dev-сервер PWA | `http://localhost:5174` | — |
