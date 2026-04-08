FROM rust:1.90-bookworm@sha256:3914072ca0c3b8aad871db9169a651ccfce30cf58303e5d6f2db16d1d8a7e58f AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY proto/ proto/
COPY crates/ crates/
COPY services/ services/
COPY tools/ tools/

RUN cargo build --release \
    -p gateway \
    -p signaling \
    -p relay

FROM debian:bookworm-slim@sha256:4724b8cc51e33e398f0e2e15e18d5ec2851ff0c2280647e1310bc1642182655d AS gateway

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && adduser --disabled-password --no-create-home --gecos "" app

COPY --from=builder /app/target/release/gateway /usr/local/bin/gateway
EXPOSE 9100 9101 9090
ENV P2P_GATEWAY_ADDR=0.0.0.0:9100 \
    P2P_WS_ADDR=0.0.0.0:9101
USER app
CMD ["gateway"]

FROM debian:bookworm-slim@sha256:4724b8cc51e33e398f0e2e15e18d5ec2851ff0c2280647e1310bc1642182655d AS signaling

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && adduser --disabled-password --no-create-home --gecos "" app

COPY --from=builder /app/target/release/signaling /usr/local/bin/signaling
RUN mkdir -p /data/signaling && chown app:app /data/signaling
EXPOSE 9200 3478/udp 9091
ENV P2P_SIGNALING_ADDR=0.0.0.0:9200 \
    P2P_STUN_ADDR=0.0.0.0:3478
WORKDIR /
USER app
CMD ["signaling"]

FROM debian:bookworm-slim@sha256:4724b8cc51e33e398f0e2e15e18d5ec2851ff0c2280647e1310bc1642182655d AS relay

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && adduser --disabled-password --no-create-home --gecos "" app

COPY --from=builder /app/target/release/relay /usr/local/bin/relay
RUN mkdir -p /data/relay && chown app:app /data/relay
EXPOSE 9300 9092
WORKDIR /
USER app
CMD ["relay"]
