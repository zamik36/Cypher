# SPEC — доводка «Шифра» до production: план, статус, остаток

> Обновлено: 2026-09-27. Ветка `dev`. Коммиты делаются без упоминания AI и без Co-Authored-By.
> Отчёт по этапам 0–6 лежит в [docs/report-2026-09.md](docs/report-2026-09.md), дорожная карта — в [docs/roadmap.md](docs/roadmap.md).

## Цель
Довести Rust-код до production-уровня:
- строжайшие линтеры и отдельный прогон Miri;
- высокое покрытие тестами (unit, интеграционные, e2e) с храповиком в CI;
- производительность и устойчивость к нагрузке;
- чистый код по SOLID/DRY/KISS/YAGNI.

То же для фронтенда: строгий ESLint, vitest и Playwright.

### Решения пользователя
- **P2P и STUN удалить.** Прямое соединение раскрывает IP собеседнику, а код был мёртвым.
- **Покрытие — храповик по крейтам.** crypto, wire, core и types — не ниже 90 %, остальные — не ниже 80 %. Порог только растёт.
- **Линтеры сразу в режиме deny, чиним всё.** Исключения — только `#[expect(lint, reason = "…")]`, а `#[allow]` запрещён линтом.
- **Фронтенд тоже:** ESLint strict-type-checked, Prettier, vitest, Playwright.

## Утверждённый план (фазы A → H)
| Фаза | Содержание | Статус |
|---|---|---|
| A | Удаление P2P и STUN, исправление секретов NATS в деплое, закрепление toolchain 1.98.1, обновление Justfile | ✅ `1e27b5c` |
| B | Сервисы разделены на lib и тонкий bin (`run(config, shutdown)`), общий `cypher_transport::server::serve`, метрики на экземпляр, `OnionUpstream` в relay, чистый модуль команд в wasm, `effects.ts` в PWA | ✅ `c872cda` |
| C | Строгие линтеры Rust: все нарушения исправлены, конфиг включён | 🔶 в работе, **не закоммичено** |
| D | Инструменты и CI: nextest (+ doctest), Miri, machete, typos, taplo, cargo-hack, ужесточение deny, nightly-workflow (Miri, fuzz 10 мин, mutants, бенчмарки, нагрузка 10k) | ⏳ |
| E | Покрытие: `cargo llvm-cov nextest` + `tools/xtask coverage-gate` + `.config/coverage.toml` (floor/target, `--bump`); тесты для tls, transport, relay, signaling (живой Redis, env `CYPHER_TEST_REDIS`), gateway, wasm, desktop (`tauri::test`), ветки ошибок | ⏳ |
| F | Производительность: criterion (crypto, wire, core, gateway, media); load-test по модулям с `--json`, `--src-ips`, `--metrics-url`, `--assert-*`, байтами на соединение из `process_resident_memory_bytes`; профилирование; `docs/performance.md` с честными целями | ⏳ |
| G | Фронтенд: строгий tsconfig (`noUncheckedIndexedAccess`, `exactOptionalPropertyTypes` и др.), ESLint и Prettier, vitest (сторы, i18n, idb на fake-indexeddb, `effects.ts`), Playwright из scratch-сценариев journey и media | ⏳ |
| H | Документация: заново `architecture.md`, `protocol-v2.md`, `threat-model.md`; удалить `onion-routing.md`; обновить README, deploy, commands; новый `CONTRIBUTING.md`; обновить roadmap и report | ⏳ |

Полный текст плана: `C:\Users\Ilya\.claude\plans\async-spinning-neumann.md`.

## Что сделано в этой серии
- **`1e27b5c` — фаза A.**
  - Удалены `crates/cypher-nat`, STUN в signaling, UDP 3478 и `P2P_SIGNALING_ADDR`.
  - `deploy.yml` и `setup.yml` передают `GATEWAY/SIGNALING/RELAY_NATS_PASSWORD` и отказываются деплоить с пустым секретом. **Нужно завести эти три секрета в GitHub.**
  - Закреплены `rust-toolchain.toml` 1.98.1, `rust:1.98-bookworm@sha256:93ce…`, а в CI — `RUST_TOOLCHAIN` и `NIGHTLY=nightly-2026-09-20`.
  - Justfile использует `.env` и NATS-пользователей, dev-сертификаты кладёт в `target/dev-certs`.
- **`c872cda` — фаза B.** Всё, что перечислено в таблице. Новые тесты:
  - `crates/cypher-transport/tests/server.rs`: лимит соединений, зависший handshake, shutdown;
  - `services/relay/src/tests.rs`: 5 тестов;
  - `crates/cypher-wasm/src/command.rs`: 4 теста;
  - тесты метрик в server-kit.
- **Раньше:** этапы 0–6 (протокол v2, ядро, сервисы, нативный и WASM-клиенты, голосовые и кружочки), см. отчёт.

## Фаза C — текущее состояние (рабочее дерево, НЕ закоммичено)
**Конфиг уже внесён:**
- в корневой `Cargo.toml`: `[workspace.lints]` (rust + clippy pedantic/restriction/nursery) и `overflow-checks = true` в release;
- новые файлы `clippy.toml` и `rustfmt.toml`.

Из плана сознательно убраны линты, которые давали шум без пользы:
- `let_underscore_drop`;
- `tests_outside_test_module` — ложные срабатывания на `tests/`;
- `partial_pub_fields` — в core поля «публичные данные + приватные инварианты» сделаны так намеренно.

**Уже чисто при новом конфиге:**
- **`cypher-types`**: `to_vec`/`to_hex` берут `self`, base32 без индексации.
- **`cypher-wire`**:
  - `Reader` на `first_chunk`/`advance`;
  - `field_len` — единственный `expect` с reason;
  - `peek_send` на `split_first_chunk`;
  - `#![cfg_attr(not(test), deny(arithmetic_side_effects, wildcard_enum_match_arm))]`;
  - saturating-подсказки ёмкости.
- **`cypher-crypto`**:
  - `kdf::{hkdf, hmac_sha256, split}` — `expect` с reason;
  - `id()` у prekey, `signed_message` через `concat`;
  - `identity_file::open` через `Reader`;
  - `LARGEST_BUCKET`, `AAD_FIXED_LEN`;
  - `#![cfg_attr(not(test), deny(arithmetic_side_effects))]`.

  Все 40 тестов зелёные.
- **`cypher-core`, почти готово:**
  - сделано: `relay.rs` переписан (`take::<N>`, `send_header() -> [u8; N]`, `write_chunk_headers(&mut [u8; CHUNK_HEADROOM], …)`); `send_file(NewFile)`; `restore(&Snapshot)` с `load_peers`/`load_outbox`/`load_transfers`; `mark_read(&[MsgId])`; `on_send_ack(&Bytes)`; `Prekeys::from_record` без `Result`; `Bitmap` через `get`/`get_mut`; `fs_name` без срезов строк; `media.rs` без кастов и срезов; `store`/`envelope` — `expect` с reason. Тесты core зелёные: 24 + 19.
  - осталось:
    1. `core/files.rs::send_file` — 70 строк при лимите 60: вынести валидацию и построение `FileDesc` в функции.
    2. `core/messaging.rs:214` — лишний `&body`.
    3. `RelayBody::decode(body: Bytes)` → `&Bytes`; вызовы — `messaging.rs:378` и тесты в `relay.rs`.
    4. `transfer.rs` — тест `receiver_validates_and_acks` (сложность 21): разбить на два.
    5. `ui.rs::event` — 72 строки: вынести ветки Transfer* в отдельную функцию; для прогресса — `#[expect(clippy::cast_precision_loss, reason = "UI ratio; exact below 2^52 bytes")]`.
    6. В `tests/scenarios.rs` — точный набор `#![expect(...)]` для тестового кода (indexing_slicing, panic, unreachable, unwrap/expect, cast_possible_truncation, too_many_lines — только те, что реально срабатывают). Убрать `#![allow(dead_code)]` в `tests/harness/mod.rs` вместе с мёртвыми хелперами. Wildcard-match в harness — заменить на явные варианты.
    7. Добавить `#![deny(clippy::wildcard_enum_match_arm)]` в `crates/cypher-core/src/lib.rs` и исправить найденное.
- **Остальные крейты.** Число нарушений — по последнему отчёту:

  | Крейт | Нарушений |
  |---|---|
  | media | 45 |
  | desktop | 27 |
  | client | 19 |
  | wasm | 16 |
  | load-test | 11 |
  | server-kit | 11 |
  | gateway | 8 |
  | transport | 2 |
  | signaling | 1 |

  Подсказки:
  - **media:** касты в DSP — одна функция `sample_to_i16` с `#[expect(cast_possible_truncation, reason)]`; в `opus.rs` — `#![expect(unsafe_code, reason = "sole boundary to unsafe-libopus")]` и SAFETY-комментарии по одному на блок.
  - **desktop, wasm:** модульный `#![expect(clippy::needless_pass_by_value, reason = "IPC/ABI requires owned args")]`; `main`/`run` без `expect`.
  - **load-test:** `writeln!` вместо `println!`, либо `expect(print_stdout)` на уровне крейта с reason.
  - **wasm `command.rs`:** в `file_size` добавить `#[expect(cast_possible_truncation, cast_sign_loss, reason = "checked: whole, non-negative, < 2^53")]` на `let exact`.
  - **gateway `bus.rs:83`:** `trivial_casts` — заменить каст на коэрсию через `let f: fn(...) -> BusMsg = convert;`.
- **Добавить `.gitattributes`** с `* text=auto eol=lf` и выполнить `git add --renormalize .`.
- **Коммитить группами после полной зелени:**
  1. конфиг + types/wire/crypto;
  2. core;
  3. media/client/wasm;
  4. transport/tls/server-kit;
  5. services;
  6. desktop/tools.

### Полезные команды
```sh
# Полный отчёт о нарушениях без остановки на первом крейте (отдельный target):
CARGO_TARGET_DIR=target/lint RUSTFLAGS="--cap-lints warn" cargo clippy --workspace --all-targets --all-features --message-format=short 2>&1 | grep -E "^(crates|services|tools|apps)" | sort -u
# Проверка одного крейта:
CARGO_TARGET_DIR=target/lint cargo clippy -p <crate> --all-targets --all-features
# Финальная проверка фазы:
cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo clippy -p cypher-desktop --all-targets -- -D warnings
cargo clippy -p cypher-wasm -p cypher-media --target wasm32-unknown-unknown -- -D warnings
cargo test --workspace --all-features
```
**Грабли:**
- Неисполненный `#[expect]` — тоже ошибка, поэтому `expect` ставится точечно.
- Атрибут нельзя вешать на выражение — только на `let`-оператор или на функцию.
- Heredoc в bash ломает `\n` и кавычки: правки удобнее писать python-скриптом через инструмент Write.

### Локальный стек для e2e-проверок
- Контейнеры:
  - `cypher-it-redis`: `docker run -d --rm --name cypher-it-redis -p 127.0.0.1:16379:6379 redis:7-alpine redis-server --requirepass itpass`;
  - `cypher-it-nats`: запуск с `MSYS_NO_PATHCONV=1`, `-v "C:/Users/Ilya/student/p2p/deploy/nats.conf:/etc/nats/nats.conf:ro"`, env `GATEWAY_NATS_PASSWORD=gwpass`, `SIGNALING_NATS_PASSWORD=sigpass`, `RELAY_NATS_PASSWORD=relpass`, порт `127.0.0.1:14222:4222`.
- Сервисы — `…/scratchpad/it/start.sh`. Порты: gateway 19100/19101, gateway2 19110, relay 19300/19301.
- Живые тесты:
  - `CYPHER_LIVE_GATEWAY=localhost:19100 CYPHER_LIVE_CA=<it>/stack.pem cargo test --release -p cypher-client --test live`;
  - `node apps/pwa/scripts/live.mjs` с `CYPHER_WS_*`;
  - браузерные сценарии `scratchpad/browser/{journey,media}.mjs` (puppeteer-core + Chrome) против `vite` на 5174. В фазе G переносятся в Playwright.

## Открытые вопросы и хвосты вне фаз
- Завести в GitHub секреты `GATEWAY_NATS_PASSWORD`, `SIGNALING_NATS_PASSWORD`, `RELAY_NATS_PASSWORD`.
- Поставлять lyrebird (мосты Tor) вместе с десктопом.
- Проверить голос и кружочки на реальных macOS и Android.
- Входящее медиа воспроизводится только после полной загрузки: сервер Range уже поддерживает, не хватает поддержки в UI.
