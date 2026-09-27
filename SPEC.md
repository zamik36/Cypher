# SPEC — доводка «Шифра» до production: план, статус, остаток

> Обновлено: 2026-09-27 (фаза C завершена). Ветка `dev`. Коммиты делаются без упоминания AI и без Co-Authored-By.
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
| C | Строгие линтеры Rust: все нарушения исправлены, конфиг включён | ✅ `969d2af`…`93c98a4` + конфиг |
| D ⏭ | Инструменты и CI: nextest (+ doctest), Miri, machete, typos, taplo, cargo-hack, ужесточение deny, nightly-workflow (Miri, fuzz 10 мин, mutants, бенчмарки, нагрузка 10k) | ⏳ |
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

## Фаза C — итог
**Конфиг:**
- `[workspace.lints]`: rust + clippy `all`/`pedantic` в режиме deny, отобранные restriction- и nursery-линты;
- `clippy.toml`: пороги и `disallowed-methods`;
- `rustfmt.toml`;
- `overflow-checks = true` в release.

Исключения — только `#[expect(lint, reason = "…")]` на самом узком месте. `#[allow]` запрещён (`allow_attributes`).

**Точечная строгость:**
- `cypher-wire` — запрет неявной арифметики (`arithmetic_side_effects`) и catch-all веток `_` в `match` по enum (`wildcard_enum_match_arm`);
- `cypher-crypto` — запрет неявной арифметики;
- `cypher-core` — запрет catch-all веток.

Везде только для продового кода (`cfg_attr(not(test), …)`).

**Из плана сознательно убраны:**
- `let_underscore_drop` — шумит на намеренных `let _ = tx.send(..)`; реальные баги ловят `let_underscore_lock`/`future`;
- `tests_outside_test_module` — ложно срабатывает на `tests/`;
- `partial_pub_fields` — поля «публичные данные + приватные инварианты» сделаны так намеренно (например, `MediaKey` скрывает ключ).

**Приёмы, которые теперь в коде:**
- Разбор недоверенных данных — `first_chunk`/`split_first_chunk`/`take::<N>` вместо индексов.
- Заголовки — массивы `[u8; N]` с константными индексами.
- Подсказки ёмкости — saturating-арифметика.
- Числовые преобразования медиа собраны в `cypher-media/src/num.rs`: каждое с reason, почему потеря допустима для аудио.
- Ресемплер работает без кастов позиции: целый индекс плюс дробная часть.
- Инварианты, которые не выразить типом, — одна функция с `expect(reason)`: `wire::codec::field_len`, `crypto::kdf::{hkdf, hmac_sha256}`, postcard в vault/envelope/ratchet/driver.
- Код, сгенерированный макросами (`#[tauri::command]` → `unreachable!`, `generate_context!` → `process::exit`), — модульный или точечный `expect`.

**Попутные улучшения:**
- `Core::restore(&Snapshot)` с `load_peers`/`load_outbox`/`load_transfers`.
- `send_file(NewFile)`: `accepts`/`describe`/`start_outgoing`.
- Исчерпывающие `match` по `ServerMsg`, `ErrorCode` и `Event`.
- `relay.rs` переписан.
- `NatsConfig { url, user, password, token }` (serde-имена `nats_*` сохранены).
- Desktop:
  - `run()` возвращает `tauri::Result`, `main` — `ExitCode`;
  - guard мьютекса больше не держится через `.await`;
  - `unique_path` не перезаписывает файл и не ищет бесконечно.
- wasm: `historyRange` — свободная функция; `SendFile(JsFile)`.
- Живой тест клиента разбит на 6 этапов.

**Проверено:**
- `fmt`, clippy `-D warnings`: workspace, desktop, wasm32, комбинации фич media, Android arm64, Linux (Docker);
- 39 наборов тестов;
- живые сценарии: нативный клиент, WASM через WebSocket, браузерные journey и media.

## Следующий шаг — фаза D (инструменты и CI)
По плану: nextest, doctest, Miri-job без кэша и секретов, machete, typos, taplo, cargo-hack, ужесточение deny, nightly-workflow.

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
