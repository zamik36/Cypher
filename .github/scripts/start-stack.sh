#!/usr/bin/env bash
# Starts the live stack for CI: Redis and NATS (per-service ACL) in Docker,
# signaling, two gateway nodes and the relay from target/release. Logs and
# the development certificates go to e2e/:
#   stack.pem     gateway 1 + relay (client journeys)
#   gateways.pem  both gateways (cross-node load)
# Ports: gateway 9100 (WS 9101, metrics 9090), gateway 9110 (WS 9111, metrics 9095),
# relay 9300 (WS 9301).
set -euo pipefail

docker run -d --name redis -p 6379:6379 redis:7-alpine@sha256:8b81dd37ff027bec4e516d41acfbe9fe2460070dc6d4a4570a2ac5b9d59df065 redis-server --requirepass ci
docker run -d --name nats -p 4222:4222 -v "$PWD/deploy/nats.conf:/etc/nats/nats.conf:ro" \
  -e GATEWAY_NATS_PASSWORD=gw -e SIGNALING_NATS_PASSWORD=sig -e RELAY_NATS_PASSWORD=rel \
  nats:2-alpine@sha256:1cfc36e2e5e638243d8c722f72c954cd0ec4b15ee82fadbc718ce12e2b3c1652 -c /etc/nats/nats.conf

mkdir -p e2e
export P2P_NATS_URL=nats://127.0.0.1:4222
# The load tests open thousands of connections from this one address.
export P2P_MAX_CONNECTIONS_PER_IP=100000
bin=target/release

P2P_NATS_USER=signaling P2P_NATS_PASSWORD=sig P2P_REDIS_URL=redis://:ci@127.0.0.1:6379 \
  P2P_ONION_KEY_PATH=e2e/onion.bin P2P_RELAY_PUBLIC_ADDR=localhost:9300 P2P_METRICS_ADDR=127.0.0.1:9091 \
  "$bin/signaling" > e2e/signaling.log 2>&1 &
P2P_NATS_USER=gateway P2P_NATS_PASSWORD=gw P2P_GATEWAY_ADDR=127.0.0.1:9100 P2P_WS_ADDR=127.0.0.1:9101 \
  P2P_METRICS_ADDR=127.0.0.1:9090 P2P_DEV_CERT_OUT=e2e/gw1.pem \
  "$bin/gateway" > e2e/gateway1.log 2>&1 &
P2P_NATS_USER=gateway P2P_NATS_PASSWORD=gw P2P_GATEWAY_ADDR=127.0.0.1:9110 P2P_WS_ADDR=127.0.0.1:9111 \
  P2P_METRICS_ADDR=127.0.0.1:9095 P2P_DEV_CERT_OUT=e2e/gw2.pem \
  "$bin/gateway" > e2e/gateway2.log 2>&1 &
P2P_NATS_USER=relay P2P_NATS_PASSWORD=rel P2P_RELAY_ADDR=127.0.0.1:9300 P2P_WS_ADDR=127.0.0.1:9301 \
  P2P_METRICS_ADDR=127.0.0.1:9092 P2P_DEV_CERT_OUT=e2e/relay.pem \
  "$bin/relay" > e2e/relay.log 2>&1 &

# Every listener up (certificates are written before binding).
for port in 9090 9091 9100 9101 9110 9111 9300 9301; do
  for _ in $(seq 100); do
    (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null && break
    sleep 0.1
  done
done

# Every service ready: listeners bound and NATS connected.
for port in 9090 9091 9092 9095; do
  for _ in $(seq 100); do
    curl -sf "http://127.0.0.1:$port/ready" > /dev/null && break
    sleep 0.1
  done
  curl -sf "http://127.0.0.1:$port/ready" > /dev/null || { echo "service on :$port not ready" >&2; exit 1; }
done

cat e2e/gw1.pem e2e/relay.pem > e2e/stack.pem
cat e2e/gw1.pem e2e/gw2.pem > e2e/gateways.pem
