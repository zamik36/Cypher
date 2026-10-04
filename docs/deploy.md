# Развёртывание

Как поднять серверную часть «Шифра» на одном VPS с Docker Compose. Известные ограничения текущего деплоя — откат, единые точки отказа, перечитывание сертификатов — перечислены в конце и закрываются этапом 2 [roadmap](roadmap.md).

## Что нужно

- Linux-хост с Docker и Compose (Ubuntu 22.04+), 2+ vCPU, 4+ ГБ памяти, 20+ ГБ диска.
- Домен с A-записью на сервер.
- Открытые порты:
  - `80`, `443` — Caddy: PWA, `/ws`, `/relay`;
  - `9100` — gateway для нативных клиентов;
  - `9300` — relay для нативных клиентов.

## Схема

```text
Интернет
  ├── :443 (HTTPS) ──► Caddy ──► PWA (статика)
  │                          ├─► /ws      ──► gateway :9101 (WebSocket)
  │                          ├─► /relay   ──► relay   :9301 (WebSocket)
  │                          └─► /grafana ──► Grafana
  ├── :9100 (TLS) ──────────────► gateway :9100
  └── :9300 (TLS) ──────────────► relay   :9300
                    внутренняя сеть: NATS (у каждого сервиса свой пользователь),
                    Redis (только signaling), Prometheus, Loki, Alloy
```

Состав:
- `docker-compose.yml` — сервисы и инфраструктура;
- `docker-compose.prod.yml` — production-слой: образы из GHCR, Caddy с Let's Encrypt, сертификаты Caddy для портов 9100/9300, лимиты ресурсов, мониторинг.

## Ручная установка

```bash
curl -fsSL https://get.docker.com | sh && sudo usermod -aG docker $USER   # затем перелогиниться
git clone https://github.com/zamik36/Cypher.git ~/cypher && cd ~/cypher
cp .env.example .env
```

В `.env` заполните каждое значение; генерируйте пароли через `openssl rand -base64 32`.

| Переменная | Назначение |
|---|---|
| `DOMAIN` | Домен; Caddy получает для него сертификат. |
| `REDIS_PASSWORD` | Пароль Redis. |
| `GATEWAY_NATS_PASSWORD`, `SIGNALING_NATS_PASSWORD`, `RELAY_NATS_PASSWORD` | Пароли пользователей NATS. Права каждого — в `deploy/nats.conf`. |
| `GRAFANA_PASSWORD` | Пароль администратора Grafana. |
| `GHCR_REPO` | Репозиторий образов, например `ghcr.io/zamik36/cypher`. |

В production-слое адрес relay для клиентов (`${DOMAIN}:9300`) и пути к сертификатам задаются самим overlay. `RELAY_PUBLIC_ADDR` и `TLS_*` из `.env` используются только без него, при локальном запуске.

### Лимиты соединений

Gateway и relay ограничивают число соединений всего (`P2P_MAX_CONNECTIONS`) и с одного адреса клиента (`P2P_MAX_CONNECTIONS_PER_IP`, по умолчанию 128). Адрес — это IPv4 или сеть IPv6 /64. Запас большой намеренно: за одним адресом мобильного оператора (CGNAT) бывает много пользователей.

- **TLS (9100, 9300):** считается адрес TCP-соединения, до рукопожатия.
- **WebSocket за Caddy:** считается адрес из последней записи `X-Forwarded-For`, которую дописал Caddy. Заголовку верят, только если соединение пришло с loopback или из частной сети (сеть Docker). Порты WebSocket в production наружу не публикуются.
- **IPv6 и Docker:** если сервер принимает IPv6, а сеть compose только IPv4, Docker проксирует такие соединения через `docker-proxy`, и все они приходят с адреса шлюза сети. Тогда либо включите IPv6 в сети compose, либо поднимите `P2P_MAX_CONNECTIONS_PER_IP`.

Запуск:

```bash
docker compose -f docker-compose.yml -f docker-compose.prod.yml up -d
docker compose -f docker-compose.yml -f docker-compose.prod.yml ps
```

Проверка:

```bash
curl -s localhost:9090/metrics | head -3   # gateway
curl -s localhost:9091/metrics | head -3   # signaling
curl -s localhost:9092/metrics | head -3   # relay
docker compose exec redis sh -c 'redis-cli -a "$REDIS_PASSWORD" ping'
```

## Автоматический деплой из GitHub

| Workflow | Когда | Что делает |
|---|---|---|
| `ci.yml` | push и PR | Проверки, тесты, e2e, нагрузка; на push в основные ветки — сборка и публикация образов в GHCR. |
| `setup.yml` | вручную | Первая установка на VPS: клонирует репозиторий, пишет `.env` из секретов, поднимает стек. |
| `deploy.yml` | после успешного CI на `main` | Обновляет код и образы на VPS, проверяет здоровье, при неудаче откатывается. |
| `release.yml` | тег `v*` | Образы с версией, сборки десктопа и Android в GitHub Release. |

Секреты репозитория:

| Секрет | Для чего |
|---|---|
| `VPS_HOST`, `VPS_USER`, `SSH_PRIVATE_KEY` | SSH-доступ к серверу. |
| `DEPLOY_PATH` | Каталог проекта на сервере (по умолчанию `~/cypher`). |
| `GHCR_TOKEN` | Чтение образов из GHCR на сервере. |
| `DOMAIN`, `REDIS_PASSWORD`, `GRAFANA_PASSWORD` | Как в `.env`. |
| `GATEWAY_NATS_PASSWORD`, `SIGNALING_NATS_PASSWORD`, `RELAY_NATS_PASSWORD` | Как в `.env`. Без них деплой откажется стартовать. |
| `ANDROID_KEYSTORE_BASE`, `ANDROID_KEYSTORE_PASSWORD`, `ANDROID_KEY_ALIAS`, `ANDROID_KEY_PASSWORD` | Подпись Android-сборки в релизе. |

## Обновление вручную

```bash
cd ~/cypher
git pull
docker compose -f docker-compose.yml -f docker-compose.prod.yml pull
docker compose -f docker-compose.yml -f docker-compose.prod.yml up -d
```

## Что хранится и что бэкапить

- **Onion-ключ signaling** — `/data/signaling/onion_key.bin` в томе `signaling-data`, создаётся при первом запуске. Им клиенты запечатывают анонимные запросы к inbox. Если его потерять, клиенты просто получат новый ключ при следующем подключении, но сохранять том между обновлениями нужно.
- **Redis** — том `redis-data`, append-only. В нём prekeys (30 дней), ссылки (24 ч) и офлайн-inbox (14 дней); всё либо публично, либо зашифровано end-to-end.
- **Мониторинг** — тома Prometheus, Grafana и Loki.

Gateway и relay состояния не хранят. Регулярных бэкапов в поставке пока нет — это задача 2.8 roadmap.

## Мониторинг

В production-слое Prometheus, Grafana, Loki и Alloy поднимаются вместе со стеком:
- Grafana доступна по `https://<DOMAIN>/grafana/`, дашборды подключаются автоматически;
- логи контейнеров собирает Alloy и складывает в Loki; сервисы пишут JSON (`LOG_FORMAT=json`).

Для мониторинга при локальной разработке:

```bash
docker compose -f docker-compose.yml -f docker-compose.monitoring.yml up -d   # Prometheus :9093, Grafana :3000
```

## Диагностика

- **Caddy не получает сертификат.** Проверьте DNS и порты 80/443: `docker compose logs caddy`.
- **Gateway не принимает соединения:** `docker compose logs gateway nats`.
- **Signaling:** `docker compose logs signaling` и проверка Redis командой выше.
- **Все логи:** `docker compose logs -f --tail=50`.

## Известные ограничения текущего деплоя

Закрываются этапами 1 и 2 [roadmap](roadmap.md):
- **Деплой и откат.** Сервисы работают на образах `:latest`, откат не возвращает предыдущие образы, деплой перезапускает весь стек.
- **Проверка здоровья.** Деплой смотрит только на ответ `/metrics`, а не на готовность сервиса.
- **Сертификаты.** Gateway и relay читают сертификат Let's Encrypt один раз при старте, поэтому после продления их нужно перезапустить. Они работают от root с доступом к тому Caddy.
- **Единые точки отказа.** Один хост, один Redis, один NATS, один gateway. Бэкапов и алертов нет; Grafana открыта наружу и защищена только паролем.
