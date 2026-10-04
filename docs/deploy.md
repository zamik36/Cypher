# Развёртывание

Как поднять серверную часть «Шифра» на одном VPS с Docker Compose. Известные ограничения текущего деплоя — единые точки отказа, права контейнеров, бэкапы и алерты — перечислены в конце и закрываются этапом 2 [roadmap](roadmap.md).

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
- `docker-compose.prod.yml` — production-слой: образы из GHCR, Caddy с Let's Encrypt, сертификаты Caddy для портов 9100/9300, лимиты ресурсов, мониторинг, раздельные сети.

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
| `IMAGE_TAG` | Какие образы запускать: первые 8 символов sha коммита, который собрал CI. Деплой из GitHub пишет его сам. |

В production-слое адрес relay для клиентов (`${DOMAIN}:9300`) и пути к сертификатам задаются самим overlay. `RELAY_PUBLIC_ADDR` и `TLS_*` из `.env` используются только без него, при локальном запуске.

### Изоляция контейнеров

- **Пользователь.** Gateway, signaling и relay работают от непривилегированного пользователя (uid 10001), с файловой системой только для чтения и без capabilities.
- **Сертификаты.** Gateway и relay не видят данные Caddy (там же ключ аккаунта ACME). Сервис `certs` (`deploy/sync-certs.sh`) раз в минуту копирует только сертификат и ключ домена в том `certs`, доступный uid 10001 на чтение; продление сервисы подхватывают сами.
- **Сети.** `internal` — Redis, NATS и сервисы; `edge` — то, к чему проксирует Caddy; `monitoring` — Prometheus, Grafana, Loki, Alloy; `docker-api` — Alloy и прокси Docker API, без выхода наружу. Redis и NATS недоступны ни Caddy, ни мониторингу.
- **Docker API.** Alloy читает логи через `docker-proxy` — только чтение контейнеров и сетей; сокет Docker смонтирован лишь в прокси.
- **Ресурсы.** Gateway — 768 МБ и не больше `GATEWAY_MAX_CONNECTIONS` соединений (по умолчанию 20 000, около 25 КБ на соединение по [performance.md](performance.md)); поднимайте оба значения вместе. `nofile` — 65 536.

**Обновление со старых версий.** Раньше сервисы работали от uid 1000. Том `signaling-data` с onion-ключом нужно один раз передать новому пользователю, иначе signaling не прочитает ключ и деплой откатится:

```bash
docker run --rm -v cypher_signaling-data:/data alpine chown -R 10001:10001 /data
```

Имя тома зависит от имени проекта compose: `docker volume ls | grep signaling`.

### Лимиты соединений

Gateway и relay ограничивают число соединений всего (`P2P_MAX_CONNECTIONS`) и с одного адреса клиента (`P2P_MAX_CONNECTIONS_PER_IP`, по умолчанию 128). Адрес — это IPv4 или сеть IPv6 /64. Запас большой намеренно: за одним адресом мобильного оператора (CGNAT) бывает много пользователей.

- **TLS (9100, 9300):** считается адрес TCP-соединения, до рукопожатия.
- **WebSocket за Caddy:** считается адрес из последней записи `X-Forwarded-For`, которую дописал Caddy. Заголовку верят, только если соединение пришло с loopback или из частной сети (сеть Docker). Порты WebSocket в production наружу не публикуются.
- **IPv6 и Docker:** если сервер принимает IPv6, а сеть compose только IPv4, Docker проксирует такие соединения через `docker-proxy`, и все они приходят с адреса шлюза сети. Тогда либо включите IPv6 в сети compose, либо поднимите `P2P_MAX_CONNECTIONS_PER_IP`.

Запуск. Production-слой ничего не собирает на сервере: если образа с `IMAGE_TAG` нет в GHCR, `pull` завершится ошибкой.

```bash
docker compose -f docker-compose.yml -f docker-compose.prod.yml pull
docker compose -f docker-compose.yml -f docker-compose.prod.yml up -d --wait
docker compose -f docker-compose.yml -f docker-compose.prod.yml ps
```

`--wait` ждёт, пока healthcheck каждого сервиса станет зелёным. Healthcheck gateway, signaling и relay — команда `<сервис> health` внутри образа: она спрашивает `/ready` на порту метрик. Сервис готов, когда открыл свои порты и подключился к NATS (signaling — ещё и подписался на запросы), и остаётся готовым, пока NATS на связи.

Проверка:

```bash
curl -s localhost:9090/ready   # gateway: ready
curl -s localhost:9091/ready   # signaling
curl -s localhost:9092/ready   # relay
docker compose exec redis redis-cli ping   # PONG; пароль берётся из REDISCLI_AUTH
```

## Автоматический деплой из GitHub

| Workflow | Когда | Что делает |
|---|---|---|
| `ci.yml` | push и PR | Проверки, тесты, e2e, нагрузка; сборка Docker-образов. На PR образы только собираются, на push публикуются с тегом коммита (`<sha8>`). Подвижные теги `latest` (main) и `dev` переставляются, только когда прошли все job'ы. |
| `setup.yml` | вручную | Первая установка: клонирует репозиторий на VPS. Стек поднимает деплой. |
| `deploy.yml` | после успешного CI на push в `main` этого репозитория; вручную с sha | Пишет `.env` из секретов с `IMAGE_TAG=<sha8>`, скачивает образы этого коммита и обновляет только изменившиеся сервисы (`up -d --wait`, без `down`). Если сервисы не стали здоровыми за 3 минуты, возвращает последний здоровый деплой — и код, и образы. Деплои идут по одному. |
| `release.yml` | тег `v*` | Ставит тег версии на образы, которые CI уже собрал и проверил для этого коммита; сборки десктопа и Android — в GitHub Release. |

Секреты репозитория:

| Секрет | Для чего |
|---|---|
| `VPS_HOST`, `VPS_USER`, `SSH_PRIVATE_KEY` | SSH-доступ к серверу. |
| `VPS_FINGERPRINT` | SHA256-отпечаток ключа SSH-хоста (`ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub` на сервере). Без него подлинность сервера не проверяется. |
| `DEPLOY_PATH` | Каталог проекта на сервере (по умолчанию `~/cypher`). |
| `GHCR_TOKEN` | Чтение образов из GHCR на сервере. |
| `DOMAIN`, `REDIS_PASSWORD`, `GRAFANA_PASSWORD` | Как в `.env`. |
| `ALERT_TELEGRAM_TOKEN`, `ALERT_TELEGRAM_CHAT_ID` | Куда слать алерты: токен бота и id чата. Необязательны. |
| `GATEWAY_NATS_PASSWORD`, `SIGNALING_NATS_PASSWORD`, `RELAY_NATS_PASSWORD` | Как в `.env`. Без них деплой откажется стартовать. |
| `ANDROID_KEYSTORE_BASE`, `ANDROID_KEYSTORE_PASSWORD`, `ANDROID_KEY_ALIAS`, `ANDROID_KEY_PASSWORD` | Подпись Android-сборки в релизе. |

## Обновление и откат вручную

Проще всего — запустить workflow **Deploy** вручную с полным sha нужного коммита: так же можно откатиться на любую версию, которую собрал CI. Без GitHub:

```bash
cd ~/cypher
git fetch origin && git checkout --detach <sha>
sed -i "s/^IMAGE_TAG=.*/IMAGE_TAG=$(printf %.8s <sha>)/" .env
docker compose -f docker-compose.yml -f docker-compose.prod.yml pull
docker compose -f docker-compose.yml -f docker-compose.prod.yml up -d --wait
```

Последний здоровый деплой записан в `.deployed-sha`.

## Что хранится и что бэкапить

- **Onion-ключ signaling** — `/data/signaling/onion_key.bin` в томе `signaling-data`, создаётся при первом запуске. Им клиенты запечатывают анонимные запросы к inbox. Если его потерять, клиенты просто получат новый ключ при следующем подключении, но сохранять том между обновлениями нужно.
- **Redis** — том `redis-data`, append-only. В нём prekeys (30 дней), ссылки (24 ч) и офлайн-inbox (14 дней); всё либо публично, либо зашифровано end-to-end.
- **Мониторинг** — тома Prometheus, Grafana и Loki.

Gateway и relay состояния не хранят. Регулярных бэкапов в поставке пока нет — это задача 2.8 roadmap.

## Мониторинг

В production-слое мониторинг поднимается вместе со стеком. Наружу ничего не публикуется: Grafana слушает `127.0.0.1:3000`, Alertmanager — `127.0.0.1:9093`. Откройте туннель `ssh -L 3000:localhost:3000 -L 9093:localhost:9093 <сервер>`.

- **Метрики.** Prometheus собирает метрики сервисов и экспортёров: Redis (`redis-exporter`), NATS (`nats-exporter`), хоста (`node-exporter`) и TLS-рукопожатий с портами 9100/9300 (`blackbox`: отвечают ли и когда истекает сертификат). Хранит 30 дней.
- **Дашборды.** «Cypher Overview» — состояние, трафик, отказы, процессы и инфраструктура; «Cypher Logs» — логи сервисов. Подключаются автоматически. Тест стека проверяет, что дашборды и алерты ссылаются только на метрики, которые сервисы действительно отдают.
- **Алерты** — в `deploy/alerts.yml`: падение сервиса, недоступный TLS, сертификат истекает (14 и 3 дня), память Redis (80 % и 95 % от `maxmemory`), NATS, диск, память хоста и контейнеров, отказы на лимитах, сброс запросов signaling. Правила проверяются в CI (`promtool test rules deploy/alerts.test.yml`).
- **Доставка.** Alertmanager шлёт алерты в Telegram: создайте бота у @BotFather, добавьте его в чат и задайте секреты `ALERT_TELEGRAM_TOKEN` и `ALERT_TELEGRAM_CHAT_ID`. Без них алерты видны только в самом Alertmanager.
- **Логи.** Alloy собирает логи контейнеров через прокси Docker API и складывает в Loki; сервисы пишут JSON (`LOG_FORMAT=json`). Loki хранит их 7 дней. Docker ротирует логи контейнеров: 3 файла по 10 МБ.

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

Закрываются этапом 2 [roadmap](roadmap.md):
- **Единые точки отказа.** Один хост, один Redis, один NATS, один gateway. Регулярных бэкапов пока нет.
- **Миграции между версиями.** Откат возвращает код и образы, но не данные: если новая версия успела записать в Redis данные нового формата, старая их не прочитает. Пока форматы Redis не менялись.
