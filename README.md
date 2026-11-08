# Шифр (Cypher)

[![Релиз](https://img.shields.io/github/v/release/zamik36/Cypher)](https://github.com/zamik36/Cypher/releases)
[![CI · dev](https://github.com/zamik36/Cypher/actions/workflows/ci.yml/badge.svg?branch=dev)](https://github.com/zamik36/Cypher/actions/workflows/ci.yml?query=branch%3Adev)
[![Лицензия](https://img.shields.io/github/license/zamik36/Cypher)](LICENSE)

Мессенджер со сквозным шифрованием для Windows, Android и браузера. Отправляйте сообщения, файлы, голосовые и видео-кружочки. Создайте личность на устройстве и пригласите собеседника по ссылке или QR-коду. Телефон и почта не нужны.

[Скачать](https://github.com/zamik36/Cypher/releases) · [Развернуть сервер](docs/deploy.md) · [Участвовать](CONTRIBUTING.md) · [Сообщить об уязвимости](SECURITY.md)

## Возможности

- Личные чаты с квитанциями доставки и прочтения, ответами с цитатой и удалением сообщений у себя.
- Шифрование файлов по чанкам и возобновление передачи после обрыва.
- Голосовые сообщения и видео-кружочки.
- Восстановление личности по фразе из 24 слов, привязка нескольких устройств и синхронизация истории.
- Ручная и автоматическая блокировка приложения, сверка кодов безопасности собеседников.
- Push-уведомления и анонимный режим с Tor в нативном клиенте.

**Бета.** Целевые платформы: Windows (Tauri 2), Android (Tauri mobile) и PWA (браузер, WebAssembly). Linux и macOS есть в матрице сборки, но не входят в текущий список поддерживаемых платформ; нативный iOS запланирован на потом.

В коде уже есть деплой с откатом, мониторинг и алерты, подпись обновлений десктопа и выпуск Android APK/AAB. Для поставки нужны настроенные секреты и аккаунты; подпись установщика Windows требует сертификата Authenticode. Наличие workflow не подтверждает готовность конкретного сервера или сборки. Оставшиеся шаги выпуска: [roadmap](docs/roadmap.md), изменения версий: [CHANGELOG](CHANGELOG.md).

## Как защищается переписка

Клиенты устанавливают сессию через X3DH и шифруют сообщения Double Ratchet. Серверы пересылают зашифрованное содержимое; для доставки офлайн временно хранят его в inbox получателя.

Онлайн-сообщения знакомым контактам тоже идут sealed sender через relay, если контакт передал токен доступа и sealed-путь доступен. Signaling и gateway получателя видят получателя, но не отправителя. Relay видит адрес клиента, но не содержимое запроса. При отсутствии получателя signaling кладёт сообщение в его inbox.

Первое сообщение до получения токена, куски файлов и их подтверждения, сообщения своим устройствам идут по аутентифицированной сессии gateway. При недоступном relay клиент может перейти на этот путь, если анонимный режим не обязателен. После сбоя sealed-доставки переход длится пять минут, после исчерпания лимита токена час, после отказа авторизации до нового `Hello` с токеном. На этом пути gateway видит отправителя и получателя.

Gateway знает IP подключённого клиента. Один relay-хоп не скрывает адрес от оператора, который контролирует и relay, и signaling. Нативный клиент в анонимном режиме подключается к relay через Tor; PWA Tor не использует. Подробности и ограничения: [модель угроз](docs/threat-model.md), [архитектура](docs/architecture.md).

## Запуск локально

### Сервер в Docker

Нужны Docker с Compose и Bash (на Windows: Git Bash).

```bash
cp .env.example .env
# Задайте свои пароли Redis и трёх пользователей NATS в .env.
docker compose up -d --build --wait
docker compose ps
docker compose exec gateway gateway health
docker compose exec signaling signaling health
docker compose exec relay relay health
```

Команды `health` проверяют `/ready` внутри контейнеров. Базовый Compose не публикует порты метрик на хост. Для Prometheus и Grafana используйте [инструкцию мониторинга](docs/deploy.md#мониторинг); не открывайте метрики в интернет. Настройку HTTPS и адреса сервера для клиентов см. в [руководстве развёртывания](docs/deploy.md).

### Разработка клиента

Нужны Rust из [rust-toolchain.toml](rust-toolchain.toml), Node.js 22, Docker, [just](https://github.com/casey/just) и Bash. Для PWA установите target `wasm32-unknown-unknown` и CLI `wasm-bindgen` версии из Cargo.lock. Для Windows и Android подготовьте [зависимости Tauri](https://v2.tauri.app/start/prerequisites/).

Сначала создайте `.env` и задайте пароли, как выше. Установите зависимости и запустите инфраструктуру:

```bash
just deps
just infra
```

В отдельном терминале оставьте сервисы работающими:

```bash
just services
```

В другом терминале выберите клиент:

```bash
just pwa-dev       # http://localhost:5174, включая сборку WASM
# или
just desktop-dev  # нужен cargo-tauri
# или
just android-dev  # нужны Android SDK/NDK и устройство либо эмулятор
```

Особенности Bash и короткого каталога сборки на Windows: [CONTRIBUTING](CONTRIBUTING.md#windows). Все рецепты: [Justfile](Justfile), пояснения: [команды](docs/commands.md).

## Проверки

В [CI](.github/workflows/ci.yml) настроены Rust-линтеры, Miri, fuzzing, проверки зависимостей и покрытия. Для интерфейса: TypeScript, ESLint, Vitest и Playwright. Сквозные сценарии используют Redis, NATS, нативный клиент и браузер.

Нагрузочный job задаёт 5000 TLS-соединений на двух gateway и 300 пар WebSocket (600 клиентов). Он проверяет пороги задержек, ошибок и ресурсов. Это параметры CI, а не обещание ёмкости production-сервера. Методика и датированные замеры: [performance.md](docs/performance.md).

## Документация и участие

| Документ                                  | Содержание                              |
| ----------------------------------------- | --------------------------------------- |
| [Архитектура](docs/architecture.md)       | Сервисы, ядро клиента и хранение        |
| [Протокол v4](docs/protocol-v4.md)        | Форматы и обмен сообщениями             |
| [Модель угроз](docs/threat-model.md)      | Защита и её границы                     |
| [Развёртывание](docs/deploy.md)           | Сервер, секреты, мониторинг и выпуск    |
| [План](docs/roadmap.md)                   | Реализованные этапы и оставшиеся задачи |
| [Историческая оценка](docs/assessment.md) | Снимок состояния на 4 октября 2026      |

Для ошибок и предложений используйте [issues](https://github.com/zamik36/Cypher/issues). Перед PR прочитайте [правила участия](CONTRIBUTING.md) и [кодекс поведения](CODE_OF_CONDUCT.md). Об уязвимостях сообщайте по [SECURITY.md](SECURITY.md).

## Лицензия

[MIT](LICENSE).
