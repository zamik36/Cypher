# Deploying Cypher to a VPS

## Requirements

- **OS**: Ubuntu 22.04+ (or any Linux with Docker)
- **Resources**: 2+ vCPU, 4+ GB RAM, 20+ GB disk
- **Ports**: 80, 443, 9100, 9300, 3478/udp
- **Domain**: A-record pointing to the server IP

## Quick Start

### 1. Install Docker

```bash
curl -fsSL https://get.docker.com | sh
sudo usermod -aG docker $USER
# Log out and back in for group change to take effect
```

### 2. Clone the repository

```bash
git clone https://github.com/<owner>/p2p.git ~/p2p
cd ~/p2p
```

### 3. Configure environment

```bash
cp .env.example .env
# Edit .env — set your domain and change default passwords:
#   DOMAIN=cypher.example.com
#   REDIS_PASSWORD=<strong random password>
#   NATS_AUTH_TOKEN=<strong random token>
```

> **Security:** Change `REDIS_PASSWORD` and `NATS_AUTH_TOKEN` from defaults before deploying to production.

### 4. Start all services

```bash
docker compose -f docker-compose.yml -f docker-compose.prod.yml up -d
```

Caddy will automatically obtain a TLS certificate from Let's Encrypt.

### 5. Verify

```bash
# Check all containers are running
docker compose ps

# Check gateway health
curl -s http://localhost:9090/metrics | head -5

# Check signaling health
curl -s http://localhost:9091/metrics | head -5

# Check relay health
curl -s http://localhost:9092/metrics | head -5
```

Visit `https://cypher.example.com` in a browser.

## Architecture

```
Internet
  │
  ├── :443 (HTTPS) ──→ Caddy ──→ PWA (static files)
  │                           ├──→ Gateway :9101 (WebSocket)
  ├── :9100 (TLS)  ──→ Caddy ──→ Gateway :9100 (native TLS)
  ├── :9300 (TLS)  ──→ Caddy ──→ Relay :9300
  └── :3478/udp    ──→ Signaling (STUN)
                          │
                     Internal network
                     ├── Redis (ephemeral state)
                     └── NATS (message routing)
```

## Updating

```bash
cd ~/p2p
git pull
docker compose pull
docker compose -f docker-compose.yml -f docker-compose.prod.yml up -d
```

Or automatically via CI/CD (push to `main` triggers deploy).

## Monitoring (optional)

```bash
docker compose -f docker-compose.yml -f docker-compose.prod.yml -f docker-compose.monitoring.yml up -d
```

- **Prometheus**: `http://localhost:9093` (internal only)
- **Grafana**: `http://localhost:3000` (default password: `admin`)

### Available Metrics

| Service    | Port | Key Metrics |
|-----------|------|-------------|
| Gateway   | 9090 | `gateway_active_connections`, `gateway_messages_routed_total`, `gateway_bytes_relayed_total` |
| Signaling | 9091 | `signaling_links_created_total`, `signaling_peer_sessions` |
| Relay     | 9092 | `relay_active_sessions`, `relay_bytes_total` |

## Custom TLS Certificates

For native TLS connections (Tauri/desktop clients connecting to port 9100/9300), you can provide your own certificates:

```bash
cat >> .env <<EOF
TLS_CERT_PATH=/path/to/cert.pem
TLS_KEY_PATH=/path/to/key.pem
EOF
```

Caddy handles HTTPS (port 443) certificates automatically.

## Notes

- **Rust version**: The Dockerfile pins `rust:1.82-bookworm` for build reproducibility. Update periodically to get compiler improvements and security fixes.
- **Prometheus metrics** (ports 9090–9092) are bound to `127.0.0.1` in production. For remote access, use an SSH tunnel: `ssh -L 9090:localhost:9090 user@server`.

## Troubleshooting

### Caddy can't get certificate
- Ensure DNS A-record points to the server
- Ensure ports 80 and 443 are open
- Check: `docker compose logs caddy`

### Gateway not accepting connections
- Check: `docker compose logs gateway`
- Verify NATS is healthy: `docker compose logs nats`

### Signaling issues
- Check Redis: `docker compose exec redis redis-cli ping`
- Check: `docker compose logs signaling`

### View all logs
```bash
docker compose logs -f --tail=50
```

## GitHub Secrets for CI/CD

| Secret | Description |
|--------|-------------|
| `VPS_HOST` | Server IP or hostname |
| `VPS_USER` | SSH username |
| `SSH_PRIVATE_KEY` | SSH private key for deployment |
| `DEPLOY_PATH` | Path to repo on server (default: `~/p2p`) |
