# Развёртывание

Как поднять серверную часть «Шифра» на одном VPS с Docker Compose. Известные ограничения текущего деплоя — единые точки отказа и миграции данных — перечислены в конце.

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
  │                          ├─► /ws    ──► gateway :9101 (WebSocket)
  │                          └─► /relay ──► relay   :9301 (WebSocket)
  ├── :9100 (TLS) ──────────────► gateway :9100
  └── :9300 (TLS) ──────────────► relay   :9300
                    внутренняя сеть: NATS (у каждого сервиса свой пользователь),
                    Redis (только signaling), Prometheus, Loki, Alloy
                    Grafana и Alertmanager — только на 127.0.0.1, через SSH-туннель
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
| `REDIS_MAXMEMORY` | Сколько памяти может занять Redis, по умолчанию `512mb`. Не меньше `512mb`, см. «Память Redis» ниже. Деплой из GitHub это значение не пишет, там действует значение по умолчанию. |
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
- **Ресурсы.** Gateway — 768 МБ и не больше `GATEWAY_MAX_CONNECTIONS` соединений (по умолчанию 20 000, около 25 КБ на соединение по [performance.md](performance.md)); поднимайте оба значения вместе. Ещё до 192 МиБ могут занять очереди на отправку клиентам: это общий бюджет узла, заданный в коде (`OUTBOX_BUDGET` в `services/gateway/src/outbox.rs`) под лимит в 768 МБ. `nofile` — 65 536.

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

### Клиенты, которые не читают

- **Gateway.** Очередь на отправку одному клиенту — до 8 МиБ, всем клиентам узла вместе — до 192 МиБ. Сверх этого кадр не ставится в очередь, и отправитель получает `Busy`. Запись клиенту ждёт не дольше 30 с; затем сессия закрывается, а ещё через 5 с прерывается и её задача записи. Так клиент, который перестал читать, не держит бесконечно ни слот соединения, ни очередь.
- **Цена.** Пока такие соединения не закрылись по таймауту, они могут занять весь бюджет, и `Busy` получат все отправители на узле. Это видно по росту `gateway_busy_total` (алерт `DeliveriesBusy`).
- **Relay.** Ответ клиенту пишется с тем же таймаутом в 30 с.

### Память Redis

Redis работает с пределом `maxmemory` (`REDIS_MAXMEMORY`, по умолчанию 512 МБ) и политикой `noeviction`: на пределе он отказывает в любой записи. Откажут ключи, ссылки, списки устройств, проверки повторов и лимиты.

Чтобы анонимные inbox не довели Redis до предела, все inbox вместе ограничены бюджетом 256 МиБ (`INBOX_BUDGET_BYTES` в `services/signaling/src/store.rs`) — половиной значения по умолчанию.
- Элемент учитывается своим размером плюс 64 байта, открытый inbox — 200 байт.
- Когда бюджет исчерпан, signaling отказывает в новых элементах и новых inbox и считает отказы в `signaling_inbox_quota_refused_total`. Если отказы идут 10 минут, приходит алерт `InboxBudgetFull`.
- Прочитанный и подтверждённый элемент возвращает свою долю сразу. Непрочитанный — когда истечёт: счётчики ведутся по дню истечения и живут до конца этого дня, так что доля возвращается не позже чем через 15 дней после записи. Плата за открытый inbox держится, пока он открыт, и возвращается при закрытии или истечении.

Как выбирать `REDIS_MAXMEMORY`:
- **Не меньше `512mb`.** Бюджет задан в коде и от `maxmemory` не зависит: с меньшим пределом inbox заполнят Redis раньше, чем сработает бюджет.
- **Больше — можно.** Остальные данные signaling растут с числом пользователей. Чтобы отдать inbox больше места, поднимите вместе с `maxmemory` и `INBOX_BUDGET_BYTES`.
- Алерты `RedisMemoryHigh` и `RedisMemoryFull` срабатывают на 80 % и 95 % от `maxmemory`.

### Переменные сервисов

Сервисы читают настройки из переменных `P2P_*` и из необязательного `config.toml` в рабочем каталоге. В compose нужные переменные уже заданы; таблица — для запуска вне compose и для тонкой настройки.

| Переменная | Сервис | По умолчанию | Назначение |
|---|---|---|---|
| `P2P_NATS_URL` | все | `nats://127.0.0.1:4222` | Адрес NATS. |
| `P2P_NATS_USER`, `P2P_NATS_PASSWORD` | все | — | Пользователь NATS из `deploy/nats.conf`. Ответы ему приходят на свой префикс `_INBOX_<пользователь>`. |
| `P2P_NATS_TOKEN` | все | — | Общий токен NATS для разработки. Пользователь с паролем важнее токена. |
| `P2P_METRICS_ADDR` | все | `0.0.0.0:9090` / `:9091` / `:9092` | Метрики и `/ready` gateway, signaling и relay. |
| `P2P_GATEWAY_ADDR` | gateway | `0.0.0.0:9100` | TLS для нативных клиентов. |
| `P2P_RELAY_ADDR` | relay | `0.0.0.0:9300` | TLS для нативных клиентов. |
| `P2P_WS_ADDR` | gateway, relay | выключен | WebSocket для браузера, за TLS-прокси. |
| `P2P_TLS_CERT_PATH`, `P2P_TLS_KEY_PATH` | gateway, relay | — | PEM сертификата и ключа, задаются вместе. Без них сервис создаёт самоподписанный сертификат для разработки. |
| `P2P_DEV_CERT_OUT` | gateway, relay | — | Куда записать самоподписанный сертификат, чтобы локальные клиенты его закрепили. |
| `P2P_MAX_CONNECTIONS` | gateway, relay | 100 000 / 20 000 | Соединений на узел. |
| `P2P_MAX_CONNECTIONS_PER_IP` | gateway, relay | 128 | Соединений с одного адреса (IPv4 или сеть IPv6 /64). |
| `P2P_FRAMES_PER_SEC` | gateway | 2000 | Кадров в секунду на соединение. |
| `P2P_BYTES_PER_SEC` | gateway | 67 108 864 (64 МиБ) | Байт в секунду на соединение. Не меньше 524 288: кадр в 1 МиБ должен проходить за 2 с, иначе gateway не стартует. |
| `P2P_REDIS_URL` | signaling | `redis://127.0.0.1:6379` | Адрес Redis с паролем. |
| `P2P_SIGNING_KEY_PATH` | signaling | `data/signing_key.bin` | Ключ подписи; создаётся при первом запуске. |
| `P2P_VAPID_KEY_PATH` | signaling | — | VAPID-ключ; создаётся при первом запуске. Без переменной push выключен. |
| `P2P_RELAY_PUBLIC_ADDR` | signaling | — | `host:port` relay, который сообщается клиентам. Без него анонимного канала нет. |
| `P2P_PUSH_CONTACT` | signaling | `https://cyphermessanger.tech` | Контакт оператора для push-сервисов (VAPID `sub`). |
| `P2P_PUSH_EXTRA_HOSTS` | signaling | — | Дополнительные push-сервисы через запятую, например свой дистрибьютор UnifiedPush. |

Логи настраиваются переменными `RUST_LOG` (по умолчанию `info`) и `LOG_FORMAT=json`.

### Запуск

Production-слой ничего не собирает на сервере: если образа с `IMAGE_TAG` нет в GHCR, `pull` завершится ошибкой.

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
| `ci.yml` | push и PR в `main` и `dev` | Проверки, тесты, e2e, нагрузка; сборка Docker-образов. На PR образы только собираются, на push публикуются с тегом коммита (`<sha8>`). Подвижные теги `latest` (main) и `dev` переставляются, только когда прошли все job'ы. |
| `setup.yml` | вручную | Первая установка: клонирует репозиторий на VPS. Стек поднимает деплой. |
| `deploy.yml` | после успешного CI на push в `main` этого репозитория; вручную с sha | Пишет `.env` из секретов с `IMAGE_TAG=<sha8>`, скачивает образы этого коммита и обновляет только изменившиеся сервисы (`up -d --wait`, без `down`). Если сервисы не стали здоровыми за 3 минуты, возвращает последний здоровый деплой — и код, и образы. Деплои идут по одному. |
| `release.yml` | тег `v*` | Ставит тег версии на образы, которые CI уже собрал и проверил для этого коммита. Публикует в GitHub Release сборки десктопа с подписанными обновлениями и `latest.json`, откуда установленные приложения узнают о новой версии, а также Android AAB (для Google Play) и APK (для установки напрямую), подписанные ключом выпуска. |
| `advisories.yml` | каждый день; вручную; push в `main` и `dev` и PR в них, если меняются `Cargo.lock`, `.deny.toml`, `crates/*/fuzz/Cargo.toml` или сам `advisories.yml` | Проверяет зависимости workspace и обоих fuzz-крейтов на уязвимости и unsound-предупреждения (`cargo deny` и `cargo audit`): по расписанию и вручную — ветки `main` и `dev`, на push и PR — их коммит. Если проверка падает не на PR, открывает issue или дописывает в уже открытый. В `ci.yml` advisories блокируют push только на уязвимостях; unmaintained, unsound и yanked там лишь предупреждения. |
| `nightly.yml` | каждую ночь | Miri, fuzzing, бенчмарки, нагрузка, мутационное тестирование и сборка Android-проекта, чтобы поломка сборки обнаруживалась до релиза. |

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
| `ANDROID_KEYSTORE_BASE64`, `ANDROID_KEYSTORE_PASSWORD`, `ANDROID_KEY_ALIAS`, `ANDROID_KEY_PASSWORD` | Подпись Android-сборок (AAB и APK) в релизе. Без них релиз не собирается. Это тот же ключ, которым подписаны выпущенные APK (с v0.1.0); его отпечаток стоит в `apps/pwa/public/.well-known/assetlinks.json`. |
| `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | Подпись обновлений десктопа. Установленное приложение ставит обновление, только если его подписал этот ключ (открытый ключ — в `tauri.conf.json`). Без них релиз не собирается. |
| `WINDOWS_CERTIFICATE`, `WINDOWS_CERTIFICATE_PASSWORD` | Сертификат Authenticode (PFX в base64) и его пароль. Необязательны: без них установщик Windows не подписан, и SmartScreen предупреждает при первом запуске. |

### Ключи выпуска

Ключ Android и ключ обновлений нельзя заменить без потерь:
- **Android.** Приложение, подписанное другим ключом, не встанет поверх установленного: пользователям придётся удалить его вместе с данными. Если в Google Play включено подписание приложений Google (Play App Signing), Google подписывает сборки своим ключом — его отпечаток (Play Console → «Целостность приложения») нужно добавить в `assetlinks.json` вторым.
- **Обновления десктопа.** С новым ключом установленные приложения перестанут принимать обновления, и новую версию придётся ставить вручную.

Храните оба ключа и их пароли офлайн, в двух местах. `assetlinks.json` должен отдаваться по `https://<домен>/.well-known/assetlinks.json` без перенаправлений: тогда ссылки-приглашения `https://<домен>/join…` открываются сразу в приложении.

**Сертификат Windows.** Его выдаёт удостоверяющий центр после проверки личности или компании. Для частного лица подходят сертификат OV у CA (ключ — на токене или в облачном HSM) или Azure Trusted Signing. Для открытого проекта можно бесплатно получить подпись через SignPath Foundation. Тогда вместо PFX в секретах будет их шаг подписи в `release.yml`.

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

**Права NATS меняются вместе с сервисами.** `deploy/nats.conf` монтируется в контейнер NATS, и права пользователей входят в протокол: с sealed-доставкой gateway публикует только `peer.*.*.gw`, а signaling — `peer.*.*.sealed`. Если версия меняет `nats.conf`, перезапустите NATS вместе с сервисами (`docker compose … up -d --force-recreate nats`), иначе sealed-сообщения не дойдут, а клиенты будут писать с именем отправителя.

**Подлинность образов.** CI подписывает каждый опубликованный образ без ключей, через Sigstore: подпись удостоверяет, что образ собрал workflow этого репозитория. К образу приложены SBOM и provenance. Проверка:

```bash
cosign verify ghcr.io/zamik36/cypher/cypher-gateway:<sha8> \
  --certificate-identity-regexp '^https://github.com/zamik36/Cypher/\.github/workflows/ci\.yml@' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

## Что хранится и бэкапы

- **Ключ подписи signaling** — `/data/signaling/signing_key.bin` в томе `signaling-data` (`P2P_SIGNING_KEY_PATH`), создаётся при первом запуске. Им сервер подписывает свои onion-ключи; клиенты запоминают его при первом подключении. **Его обязательно бэкапить**: с новым ключом клиенты не узнают сервер и останутся без анонимного канала, пока пользователь сам не доверится новому ключу.
- **Onion-ключи** — в Redis, по ключу на эпоху в 7 дней, зашифрованы ключом, выведенным из ключа подписи, и стираются через 2 дня после конца эпохи. Бэкапить их не нужно: недостающий ключ signaling создаёт сам. Чем дольше хранятся снимки Redis (AOF и бэкапы), тем дольше записанный onion-трафик можно открыть вместе с ключом подписи.
- **VAPID-ключ signaling** — `/data/signaling/vapid_key.bin` в том же томе, создаётся при первом запуске; без `P2P_VAPID_KEY_PATH` push выключен. Им подписаны сигналы Web Push; клиенты подписываются на его публичную половину. Если он сменится, устройства переподпишутся при следующем запуске, а до того сигналы до них не дойдут. Signaling ходит наружу только по HTTPS на порт 443 к известным push-сервисам (FCM, Mozilla, Windows, Apple, ntfy.sh); свой дистрибьютор UnifiedPush добавляется в `P2P_PUSH_EXTRA_HOSTS`.
- **Redis** — том `redis-data`, append-only. В нём prekeys (30 дней), ссылки (24 ч), офлайн-inbox (14 дней) и push-подписки по inbox (14 дней, без `PeerId`); всё либо публично, либо зашифровано end-to-end.
- **Мониторинг** — тома Prometheus, Grafana, Loki и Alertmanager; их не бэкапим.

Gateway и relay состояния не хранят.

**Бэкапы.** Сервис `backup` (`deploy/backup.sh`) раз в сутки снимает снапшот Redis (`redis-cli --rdb`) и архив тома `signaling-data` в том `backups` и хранит 14 дней. Время последнего успеха уходит в Prometheus; если бэкапа нет больше 26 часов, приходит алерт `BackupStale`. Список бэкапов:

```bash
docker compose -f docker-compose.yml -f docker-compose.prod.yml exec backup ls -l /backups
```

Том `backups` лежит на том же диске: от ошибок и порчи данных он спасает, от потери сервера — нет. Копируйте его за пределы хоста, например по cron через `restic` с шифрованием: в бэкапах onion-ключ.

**Восстановление** (проверено на копии стека). Скрипт останавливает signaling и Redis, возвращает оба тома к снапшоту, переписывает AOF Redis из снапшота и поднимает сервисы:

```bash
cd ~/cypher
deploy/restore.sh 20261004T030000Z
```

Что теряется: офлайн-сообщения, пришедшие после снапшота, и ссылки, созданные после него. Сессии клиентов не страдают — они хранятся на устройствах.

## Мониторинг

В production-слое мониторинг поднимается вместе со стеком. Наружу ничего не публикуется: Grafana слушает `127.0.0.1:3000`, Alertmanager — `127.0.0.1:9093`. Откройте туннель `ssh -L 3000:localhost:3000 -L 9093:localhost:9093 <сервер>`.

- **Метрики.** Prometheus собирает метрики сервисов и экспортёров: Redis (`redis-exporter`), NATS (`nats-exporter`), хоста (`node-exporter`) и TLS-рукопожатий с портами 9100/9300 (`blackbox`: отвечают ли и когда истекает сертификат). Хранит 30 дней.
- **Дашборды.** «Cypher Overview» — состояние, трафик, отказы, процессы и инфраструктура; «Cypher Logs» — логи сервисов. Подключаются автоматически. Тест стека проверяет, что дашборды и алерты ссылаются только на метрики, которые сервисы действительно отдают.
- **Алерты** — в `deploy/alerts.yml`: падение сервиса, недоступный TLS, сертификат истекает (14 и 3 дня), память Redis (80 % и 95 % от `maxmemory`), NATS, диск, память хоста и контейнеров, отказы на лимитах, сброс запросов signaling, сбой sealed-доставки. Правила проверяются в CI (`promtool test rules deploy/alerts.test.yml`).
- **Метрики лимитов.**
  - `signaling_inbox_quota_refused_total` — отказы из-за общего бюджета inbox (алерт `InboxBudgetFull`, см. «Память Redis»).
  - `signaling_push_shed_total` — push-сигналы, сброшенные сверх 256 одновременных (алерт `PushSignalsShed`). Устройство тогда найдёт сообщение при следующем опросе inbox.
  - `gateway_publish_failed_total` — вытеснения, межузловые кадры и ответы о доставке, которые не принял NATS. Алерта на неё нет: смотрите её вместе с `NatsDown` и `NatsSlowConsumers`.
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

Высокая доступность — в разделе «Позже» [roadmap](roadmap.md).
- **Единые точки отказа.** Один хост, один Redis, один NATS, один gateway. Бэкапы локальные; копирование за пределы хоста настраивается отдельно.
- **Миграции между версиями.** Откат возвращает код и образы, но не данные: если новая версия успела записать в Redis данные нового формата, старая их не прочитает. Пока форматы Redis не менялись.
