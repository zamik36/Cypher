# Deploying Cypher to a VPS

## Requirements

- OS: Ubuntu 22.04+ or another Linux host with Docker
- Resources: 2+ vCPU, 4+ GB RAM, 20+ GB disk
- Open ports: `80`, `443`, `9100`, `9300`
- Domain: A record pointing to the server IP

## Quick Start

### 1. Install Docker

```bash
curl -fsSL https://get.docker.com | sh
sudo usermod -aG docker $USER
```

Re-login after adding your user to the `docker` group.

### 2. Clone the repository

```bash
git clone https://github.com/<owner>/p2p.git ~/p2p
cd ~/p2p
```

### 3. Configure environment

```bash
cp .env.example .env
```

Set at least:

- `DOMAIN=cypher.example.com`
- `REDIS_PASSWORD=<strong random password>`
- `GATEWAY_NATS_PASSWORD`, `SIGNALING_NATS_PASSWORD`, `RELAY_NATS_PASSWORD` — one
  strong random password per service; `deploy/nats.conf` gives each service
  only the subjects it needs.
- `GRAFANA_PASSWORD=<strong random password>`
- `RELAY_PUBLIC_ADDR=<domain>:9300` — the relay address signaling advertises to clients

Generate each secret with `openssl rand -base64 32`. The GitHub deploy
workflows read the same names from repository secrets and refuse to deploy
when any of them is empty.

Change every default secret before a production deployment.

### 4. Start the stack

```bash
docker compose -f docker-compose.yml -f docker-compose.prod.yml up -d
```

Caddy will obtain TLS certificates automatically.

### 5. Verify

```bash
docker compose ps
curl -s http://localhost:9090/metrics | head -5
curl -s http://localhost:9091/metrics | head -5
curl -s http://localhost:9092/metrics | head -5
```

## Architecture

```text
Internet
  |
  +-- :443 (HTTPS) --> Caddy --> PWA static files
  |                          +--> /ws    --> Gateway :9101 (WebSocket)
  |                          +--> /relay --> Relay   :9301 (WebSocket)
  +-- :9100 (TLS)  ---------------------> Gateway :9100 (native clients)
  +-- :9300 (TLS)  ---------------------> Relay   :9300 (onion channel)
                            |
                       Internal network
                       +--> NATS  (gateway, signaling, relay; per-service users)
                       +--> Redis (signaling only)
```

## Persistent State

- **Signaling onion key** — `/data/signaling/onion_key.bin` in the `signaling-data`
  volume, created on first start. Clients seal anonymous inbox requests to it,
  so keep the volume across updates and include it in backups. Losing it only
  forces clients to fetch the new key on their next bootstrap.
- **Redis** (`redis-data` volume, append-only) — prekeys (30 days), share links
  (24 h) and offline inboxes (14 days). Everything in it is either public or
  end-to-end encrypted.

Gateway and relay are stateless.

## Updating

```bash
cd ~/p2p
git pull
docker compose pull
docker compose -f docker-compose.yml -f docker-compose.prod.yml up -d
```

## Monitoring

Optional monitoring stack:

```bash
docker compose -f docker-compose.yml -f docker-compose.prod.yml -f docker-compose.monitoring.yml up -d
```

- Prometheus: `http://localhost:9093`
- Grafana: `http://localhost:3000`

## Custom TLS Certificates

For native TLS clients on ports `9100` and `9300`, you can provide your own certificate pair:

```bash
TLS_CERT_PATH=/path/to/cert.pem
TLS_KEY_PATH=/path/to/key.pem
```

## Troubleshooting

### Caddy cannot obtain a certificate

- Verify DNS points to the server
- Verify ports `80` and `443` are open
- Check `docker compose logs caddy`

### Gateway is not accepting connections

- Check `docker compose logs gateway`
- Check `docker compose logs nats`

### Signaling problems

- Check `docker compose exec redis redis-cli ping`
- Check `docker compose logs signaling`

### View logs

```bash
docker compose logs -f --tail=50
```

## CI/CD Secrets

| Secret | Description |
|--------|-------------|
| `VPS_HOST` | Server IP or hostname |
| `VPS_USER` | SSH username |
| `SSH_PRIVATE_KEY` | SSH private key used for deployment |
| `DEPLOY_PATH` | Repository path on the server |
