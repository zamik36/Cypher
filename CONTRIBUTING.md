# Как вносить изменения

Как устроен проект — в [docs/architecture.md](docs/architecture.md), все команды — в [docs/commands.md](docs/commands.md).

## Перед коммитом

```bash
just check        # Rust: rustfmt, clippy (workspace, desktop, wasm32), taplo, typos, machete, cargo-deny, тесты
just web-check    # фронтенд: tsc, ESLint, Prettier, vitest с порогом покрытия
```

CI требует того же, плюс: покрытие, Miri, сквозные сценарии на живом стеке и нагрузку с порогами (`.github/workflows/ci.yml`).

Коммиты — небольшие и по одной теме; каждый собирается и проходит проверки сам по себе. Сообщение объясняет, что изменилось и почему.

## Правила кода

**Rust.** Линты заданы в `[workspace.lints]` корневого `Cargo.toml` и в `.clippy.toml`; предупреждение — ошибка.
- Исключение из линта — только `#[expect(lint, reason = "…")]` на самом узком месте; `#[allow]` запрещён.
- `unsafe` запрещён во всём воркспейсе.
- `unwrap`/`expect` запрещены в продовом коде. Инвариант, который нельзя выразить типом, оформляется одной функцией с `expect(reason)`.
- Разбор недоверенных данных (`cypher-wire`, `cypher-crypto`) — без неявной арифметики и без catch-all веток `_` по enum.

**Ядро.** Протокол и криптография живут только в `cypher-core` и ниже. Драйверы (нативный, WASM, Tauri) переносят байты и выполняют эффекты по порядку, но ничего не решают сами.

**Фронтенд.** Строгий TypeScript (`tsconfig.base.json`), ESLint `strictTypeChecked` плюс правила Solid, Prettier. Граница с wasm типизирована из Rust (`cypher-wasm/src/convert.rs`) — не приводите её типы вручную.

## Тесты и где их писать

| Что | Где |
|---|---|
| Логика крейта | модульные тесты рядом с кодом |
| Протокол и криптография | proptest и fuzz-цели (`crates/*/fuzz`) |
| Поведение ядра без сети | `crates/cypher-core/tests/scenarios.rs` |
| Сервисы с Redis/NATS | тесты сервисов и in-process стек `tests/e2e` |
| Команды десктопа | `apps/desktop/src-tauri/src/tests.rs`: вызов по имени через IPC, как из webview |
| Сторы и логика фронтенда | `*.test.ts` рядом с модулем (vitest, jsdom) |
| Пользовательские сценарии в браузере | `apps/pwa/e2e` (Playwright, через UI) |

**Живой стек в тестах.** Тесты с Redis и NATS запускаются, когда заданы переменные окружения; без них они пропускаются.
- `CYPHER_TEST_REDIS` — например, `redis://:pass@127.0.0.1:16379`.
- `CYPHER_TEST_NATS` — например, `nats://127.0.0.1:24222`.
- Для NATS с ACL из `deploy/nats.conf` — ещё `GATEWAY_NATS_PASSWORD`, `SIGNALING_NATS_PASSWORD` и `RELAY_NATS_PASSWORD`.

NATS нужен отдельный, без других запущенных сервисов: чужой signaling в той же очереди ответит на запросы теста.

## Покрытие — храповик

- **Rust:** `.config/coverage.toml` задаёт по каждому крейту нижний порог (`floor`) и цель (`target`). `just cov` меряет и проверяет. `just cov --bump` поднимает пороги до «замер − 1» и никогда их не опускает. Новый крейт должен попасть либо в пороги, либо в `excluded`.
- **Фронтенд:** пороги в `vitest.config.ts` по тому же правилу.
- **Откуда брать замер.** Пороги берутся из замера на Linux (как в CI): на Windows отдельные крейты отличаются примерно на пункт. Запускайте `just cov --bump` на Linux или в контейнере `rust:1.98-bookworm` с доступом к Redis и NATS.

## Сквозные сценарии локально

```bash
just infra && just services   # Redis, NATS и три сервиса
just e2e                      # нативный клиент, затем Playwright
```

Для Playwright один раз выполните `npx playwright install chromium` и `just wasm`. Отчёты и трассы падений пишутся в `target/playwright`.

## Сгенерированное

Всё, что создают сборка и тесты, пишется в `target/`: Rust, покрытие, Playwright, k6-бандл. Исключения — `dist/` приложений и `apps/pwa/src/wasm`; они тоже в `.gitignore`. В репозиторий попадают только исходники.
