# Deploying Cypher to a VPS

## Requirements

- OS: Ubuntu 22.04+ or another Linux host with Docker
- Resources: 2+ vCPU, 4+ GB RAM, 20+ GB disk
- Open ports: `80`, `443`, `9100`, `9300`, `3478/udp`
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
- `NATS_AUTH_TOKEN=<strong random token>`

Important:

- Change default Redis and NATS secrets before production deployment.
- Anonymous transport no longer needs separate onion-specific production env variables.
- Relay bootstrap and inbox signing identity are stored on disk and must survive restarts.

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
  |                          +--> Gateway :9101 (WebSocket)
  +-- :9100 (TLS)  --> Caddy --> Gateway :9100 (native TLS)
  +-- :9300 (TLS)  --> Caddy --> Relay :9300
  +-- :3478/udp    --> Signaling (STUN)
                            |
                       Internal network
                       +--> Redis
                       +--> NATS
```

## Persistent Service Keys

Relay and signaling now keep their cryptographic identity on disk:

- `data/relay/onion_identity.bin`
- `data/signaling/inbox_signing.bin`
- `data/signaling/inbox_hmac.bin`

Recommendations:

- Mount `./data` on persistent storage.
- Include these files in backups.
- Do not rotate or delete them casually.
- Keep file permissions restricted to the service user.

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
