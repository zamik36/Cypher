# Шифр (Cypher)

Мессенджер со сквозным шифрованием: сообщения, файлы любого размера, голосовые и видео-«кружочки». Без регистрации, телефона и почты — личность создаётся на устройстве и восстанавливается по фразе из 24 слов. Сервер пересылает и временно хранит только зашифрованные байты.

## Статус

**Бета.** Криптография, протокол, общее ядро клиента и серверные сервисы готовы и проверены: строгие линтеры, Miri, fuzz, покрытие 81–98 %, нагрузка с порогами в CI. Для закрытой беты проект пригоден.

Перед публичным запуском нужно закрыть блокеры:
- сверка ключа при знакомстве;
- версии хранимых данных;
- защита от засорения сервера;
- деплой с откатом;
- подписанные и самообновляемые клиенты;
- алерты.

Честная оценка — в [docs/assessment.md](docs/assessment.md), план — в [docs/roadmap.md](docs/roadmap.md).

## Как это работает

```text
 клиент ──TLS / WebSocket──► gateway ──NATS──► gateway ──► клиент
                               │
                               └──NATS──► signaling ── Redis
 клиент ──(Tor)──► relay ──NATS──┘        (ключи, ссылки, офлайн-inbox)
```

1. Первый собеседник создаёт ссылку-приглашение или QR и передаёт её второму.
2. Второй открывает ссылку, получает ключи первого и устанавливает сессию: X3DH, затем Double Ratchet. У каждого сообщения свой ключ.
3. Пока оба в сети, сообщения идут через gateway. Он знает, кто кому пишет, но видит только зашифрованное тело.
4. Если получатель не в сети, сообщение запечатывается без указания отправителя (sealed sender) и кладётся в его inbox через relay. Relay не видит запрос, signaling не видит адрес клиента. Десктоп ходит к relay через Tor.
5. Файлы шифруются по чанкам ключом конкретного файла; передача возобновляется после обрыва.

От чего система защищает и от чего пока нет — в [модели угроз](docs/threat-model.md).

## Клиенты

| Платформа | Технология |
|---|---|
| Windows, Linux, macOS | Tauri 2 + SolidJS, нативное ядро на Rust |
| Android | Tauri 2 mobile |
| Браузер, iOS | PWA: то же ядро в WebAssembly внутри Web Worker |

Интерфейс общий (`packages/ui`), ядро общее (`crates/cypher-core`): протокол и криптография одинаковы на всех платформах.

## Быстрый старт

**Сервер в Docker:**

```bash
cp .env.example .env          # задать пароли
docker compose up -d
curl -s localhost:9090/metrics | head -3   # gateway жив
```

Production с Caddy (TLS от Let's Encrypt), мониторингом и CI/CD описан в [docs/deploy.md](docs/deploy.md).

**Разработка** (нужны Rust, [just](https://github.com/casey/just), Docker, Node.js 22):

```bash
just infra                    # Redis + NATS
just services                 # gateway, signaling, relay
just deps && just pwa-dev     # PWA на http://localhost:5174
just desktop-dev              # или десктоп
```

## Качество

- **Rust.**
  - clippy `pedantic` и отобранные restriction-линты в режиме deny, `unsafe` запрещён;
  - Miri для крипто-, протокольного и медиа-кода;
  - fuzz-цели, cargo-deny;
  - храповик покрытия по крейтам: 81–98 % строк.
- **Фронтенд.** Строгий TypeScript, ESLint strict-type-checked, vitest, Playwright на продакшен-сборке.
- **Сквозные проверки.** В CI на каждый PR живой стек проходит сценарии нативного клиента и браузера, затем нагрузку: 5000 TLS-клиентов и 1000 пар WebSocket с порогами p99.

## Документация

| Документ | О чём |
|---|---|
| [docs/architecture.md](docs/architecture.md) | Устройство системы: сервисы, крейты, потоки данных, хранение |
| [docs/protocol-v2.md](docs/protocol-v2.md) | Протокол клиент ↔ сервер и end-to-end уровень |
| [docs/threat-model.md](docs/threat-model.md) | Что защищается, от кого, где границы |
| [docs/assessment.md](docs/assessment.md) | Оценка готовности: сильные стороны и проблемы по серьёзности |
| [docs/roadmap.md](docs/roadmap.md) | План по этапам до версии 1.0 |
| [docs/report-2026-09.md](docs/report-2026-09.md) | История работ: аудит, переписывание, доводка |
| [docs/performance.md](docs/performance.md) | Бенчмарки, нагрузка, профилирование |
| [docs/deploy.md](docs/deploy.md) | Развёртывание и секреты |
| [docs/commands.md](docs/commands.md) | Все команды `just` |
| [CONTRIBUTING.md](CONTRIBUTING.md) | Проверки, правила кода, тесты, храповик покрытия |

## Лицензия

MIT — [LICENSE](LICENSE).
