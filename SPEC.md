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
| D | Инструменты и CI: nextest (+ doctest), Miri, machete, typos, taplo, cargo-hack, ужесточение deny, nightly-workflow (Miri, fuzz 10 мин, mutants); Opus на эталонном libopus, `unsafe_code = "forbid"` | ✅ `66ae1e7` |
| E | Покрытие: `cargo llvm-cov nextest` + `tools/xtask coverage-gate` + `.config/coverage.toml` (floor/target, `--bump`); тесты для tls, transport, relay, signaling (живой Redis, env `CYPHER_TEST_REDIS`), gateway, wasm, desktop (`tauri::test`), ветки ошибок | ✅ `f044c26` (desktop 74% из 80 — остаток в G) |
| F | Производительность (бенчмарки и нагрузка 10k в nightly — здесь): criterion (crypto, wire, core, gateway, media); load-test по модулям с `--json`, `--src-ips`, `--metrics-url`, `--assert-*`, байтами на соединение из `process_resident_memory_bytes`; профилирование; `docs/performance.md` с честными целями | ✅ `e884ca1`…`8849b9f` |
| G ⏭ | Фронтенд: строгий tsconfig (`noUncheckedIndexedAccess`, `exactOptionalPropertyTypes` и др.), ESLint и Prettier, vitest (сторы, i18n, idb на fake-indexeddb, `effects.ts`), Playwright из scratch-сценариев journey и media | ⏳ |
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

## Фаза D — итог
**Инструменты** (версии закреплены в CI через `taiki-e/install-action`):
- `cargo-nextest` 0.9.146 — `.config/nextest.toml`, профили `default`, `ci` (junit), `miri`, `miri-full`;
- `cargo-machete`, `typos` (`_typos.toml`), `taplo` (`.taplo.toml`), `cargo-hack`;
- `cargo-deny`: `wildcards = "deny"`, бан openssl и native-tls, `unused-ignored-advisory = "deny"`. Все крейты `publish = false`.

**CI** (`.github/workflows/ci.yml`):
- job `test` — nextest `--profile ci` + doctest'ы, junit как артефакт;
- новый job `hygiene` — taplo, typos, machete, `cargo hack clippy --each-feature` для media, client и transport;
- новый job `miri` — закреплённый nightly, без кэша target, без секретов, `persist-credentials: false`, `-Zmiri-strict-provenance`;
- `permissions: contents: read` по умолчанию на весь workflow.

**Nightly** (`.github/workflows/nightly.yml`, cron):
- Miri `miri-full --run-ignored all`;
- fuzz по 10 мин на цель;
- cargo-mutants для crypto, wire и core — отчёт, без gate.

**Miri нашёл UB в `unsafe-libopus`.** Tree Borrows: запрещённый reborrow в `opus_custom_encoder_ctl_impl`, где C-шный memset идёт по хвосту структуры через указатель на поле. Что изменилось:
- голос кодирует эталонный libopus через крейт `opus` (bundled, cmake);
- в воркспейсе больше нет `unsafe`, поэтому `unsafe_code = "forbid"`;
- для Android `opusic-sys` берёт тулчейн NDK из `ANDROID_NDK_HOME` — выставлен в `release.yml`. Локально на Windows нужен `CMAKE_GENERATOR=Ninja`.

**Что Miri пропускает в PR:**
- **Отмечены `cfg_attr(miri, ignore)` с reason** — тяжёлые тесты:
  - Argon2 на 64 МиБ;
  - кадр onion максимального размера;
  - тысячи пропущенных ключей;
  - proptest с 200 операциями;
  - 20 DH-шагов.
- **Исключён `default-filter` профиля `miri`** — бинарник `cypher-core::scenarios`: до часа на тест.
- **Как запустить всё** — nightly или `just miri miri-full --run-ignored all`.
- **Proptest под Miri** — 4 случая и без файлов persistence.

**Проверено:**
- nextest — 163 теста, плюс doctest'ы;
- Miri по types, wire, crypto, core и media — UB нет;
- clippy `-D warnings`: workspace, desktop, wasm32, все 8 комбинаций фич, Android arm64 с libopus, Linux в Docker (workspace, desktop, тесты media);
- taplo, typos, machete, deny.

## Фаза E — итог
**Храповик покрытия:**
- `tools/xtask`: `cargo run -p xtask -- coverage-gate [--report] [--bump]`;
- `.config/coverage.toml`: нижний порог (`floor`) и цель (`target`) по крейтам;
- в `excluded` явно перечислены wasm, e2e, load-test и xtask. Новый крейт, не попавший ни в пороги, ни в `excluded`, — ошибка.

`--bump` ставит порог на 1 пункт ниже замера: пути с таймаутами и переподключениями дают разброс около пункта между прогонами. Порог никогда не опускается. Пороги взяты из Linux-замера, как в CI.

**CI:**
- job `coverage` поднимает Redis и NATS с ACL, меряет `cargo llvm-cov nextest --workspace`, прогоняет gate, сохраняет lcov как артефакт;
- job `test` собирает default-members, desktop тестируется в своём job;
- рецепт `just cov`.

**`tests/e2e`** — новый крейт:
- `Stack` поднимает gateway, signaling и relay в процессе теста на свободных портах с dev-сертификатами;
- `journey` — общий пользовательский сценарий;
- `tests/stack.rs` запускается при `CYPHER_TEST_REDIS` и `CYPHER_TEST_NATS`;
- `tests/live.rs` — бывший live-тест `cypher-client`, для развёрнутого стека.

Тесты со стеком объединены в nextest-группу `live-stack` (`max-threads = 1`): два стека в одной очереди NATS отвечали бы друг другу.

**Найдено тестами и исправлено:**
- `cypher-media://` отвечал 416 вместо 404 на неизвестный файл. Добавлен `ClientError::NotFound`.
- Секрет на диске (`secrets.rs`): временный файл с фиксированным именем, оставшийся после падения, навсегда блокировал старт signaling. Теперь у временного файла случайное имя.
- Ошибки подключения клиента к gateway и relay нигде не логировались. Теперь `warn` с адресом и причиной.
- Desktop-тесты на Windows не запускались (`0xc0000139`): манифест Common Controls v6 теперь встраивается во все цели через `build.rs`.

**Рефакторинг ради тестируемости:**
- **tls:** удалено неиспользуемое API (`make_client_config_with_cert`, `make_server_config`); загрузка PEM стала приватной, цикл повторов упрощён.
- **media:** захват через DIP. Поток-кодер получает `open`; downmix — чистая функция.
- **desktop:**
  - `Paths` и TLS резолвятся при старте и хранятся в `AppState`;
  - `connect` принимает sink событий и не зависит от Tauri;
  - команды обобщены по `Runtime`;
  - `VideoNote::parse` вынесен в отдельную функцию;
  - команды тестируются через mock runtime и настоящий IPC.
- **server-kit:** `NatsConfig::auth()` с явным приоритетом «user+password > token», пустые значения считаются отсутствующими.

**Linux-замер, 209 тестов** (порог = замер − 1):

| Крейт | % | Цель |
|---|---|---|
| crypto | 97.6 | 90 |
| wire | 97.2 | 90 |
| types | 96.6 | 90 |
| transport | 96.6 | 80 |
| tls | 96.3 | 80 |
| core | 92.1 | 90 |
| signaling | 91.6 | 80 |
| media | 89.4 | 80 |
| client | 88.0 | 80 |
| gateway | 87.8 | 80 |
| server-kit | 85.7 | 80 |
| relay | 81.1 | 80 |
| desktop | 74.4 | 80 |

**Desktop ниже цели.** В оставшихся 26% — запуск приложения (`run`, `main`), запись с микрофона, файловый диалог и регистрация URI-схемы. Им нужно настоящее окно или устройство, это проверяется в фазе G (WebDriver).

## Фаза F — итог
Все цифры, методика и способ воспроизведения — в `docs/performance.md`.

**Сделано:**
- **Бенчмарки на criterion** для crypto, wire, core и media: `just bench`; nightly-job `bench` сохраняет `target/criterion`. На PR бенчмарки только компилируются через `clippy --all-targets`.
- **Load-test переписан:**
  - модули;
  - `--ramp`/`--duration`;
  - перцентили p50–p999 для подключения и пересылки;
  - `--json`;
  - `--metrics-addr` — RSS gateway в пересчёте на его соединения;
  - `--src-ips`;
  - пороги `--max-errors`, `--assert-p99-ms`, `--assert-max-bytes-per-conn`.
- **Нагрузка в CI:**
  - e2e — 5k клиентов на двух узлах: 0 ошибок, p99 ≤ 50 мс, ≤ 40 КБ на соединение;
  - nightly — 10k клиентов, 60 с.
  - Стек поднимает `.github/scripts/start-stack.sh`.
- **Профилирование и оптимизация.** heaptrack показал, что память соединения — это буферы `Framed` по 8 КиБ и две подписки NATS на пира. Что изменено:
  - вытеснение старой сессии — пустое сообщение на `peer.<id>`;
  - `ctl.*` убран;
  - буферы соединения по 2 КиБ.

  Итог: **38.3 → 24.5 КБ на соединение**.
- **Утечек нет.** Соединения, дескрипторы и подписки возвращаются к исходным значениям. Рост RSS между прогонами — это удержание памяти аренами glibc. mimalloc проверен и отклонён: пиковый расход у него выше.
- **Dev-профиль:** без отладочной информации для зависимостей. `target/` больше не разрастается до 100+ ГБ.

**Linux, 10k клиентов на двух узлах:**
- 0 ошибок;
- пересылка p50 0.98 / p99 2.5 мс;
- 18.8 тыс. сообщений/с;
- 24.5 КБ на соединение.

**Микробенчмарки:**
- ratchet — 1.9 мкс на 1 КиБ;
- AES-GCM на чанках — ≈1 ГБ/с;
- X3DH — 381 мкс;
- кодирование Opus — ×183 от реального времени.

**Не сделано:**
- прогон на 100k — это ручная процедура на отдельной машине, описана в `docs/performance.md`;
- CPU-профиль под нагрузкой — узких мест по задержкам и пропускной способности не видно.

## Следующий шаг — фаза G (фронтенд)
По плану:
- строгий `tsconfig`;
- ESLint (strict-type-checked) и Prettier;
- vitest: сторы, i18n, idb на fake-indexeddb, `effects.ts`;
- Playwright из сценариев journey и media;
- WebDriver-тесты desktop для запуска приложения, микрофона и файлового диалога — это закроет цель покрытия desktop.

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
cargo nextest run --workspace --all-features
# Всё вместе: just check (lint + hygiene + test); UB: just miri
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
  - `CYPHER_LIVE_GATEWAY=localhost:19100 CYPHER_LIVE_CA=<it>/stack.pem cargo test --release -p e2e --test live`;
  - стек в процессе: отдельный NATS без других сервисов (`cypher-e2e-nats` на 24222, та же ACL), затем `CYPHER_TEST_REDIS=redis://:itpass@127.0.0.1:16379 CYPHER_TEST_NATS=nats://127.0.0.1:24222 {GATEWAY,SIGNALING,RELAY}_NATS_PASSWORD=… just cov`;
  - `node apps/pwa/scripts/live.mjs` с `CYPHER_WS_*`;
  - браузерные сценарии `scratchpad/browser/{journey,media}.mjs` (puppeteer-core + Chrome) против `vite` на 5174. В фазе G переносятся в Playwright.

## Открытые вопросы и хвосты вне фаз
- Завести в GitHub секреты `GATEWAY_NATS_PASSWORD`, `SIGNALING_NATS_PASSWORD`, `RELAY_NATS_PASSWORD`.
- Поставлять lyrebird (мосты Tor) вместе с десктопом.
- Проверить голос и кружочки на реальных macOS и Android.
- Входящее медиа воспроизводится только после полной загрузки: сервер Range уже поддерживает, не хватает поддержки в UI.
