FROM rust:1.90-bookworm AS builder

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

FROM debian:bookworm-slim AS gateway

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && adduser --disabled-password --no-create-home --gecos "" app

COPY --from=builder /app/target/release/gateway /usr/local/bin/gateway
EXPOSE 9100 9101 9090
ENV P2P_GATEWAY_ADDR=0.0.0.0:9100 \
    P2P_WS_ADDR=0.0.0.0:9101
USER app
CMD ["gateway"]

FROM debian:bookworm-slim AS signaling

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && adduser --disabled-password --no-create-home --gecos "" app

COPY --from=builder /app/target/release/signaling /usr/local/bin/signaling
EXPOSE 9200 3478/udp 9091
ENV P2P_SIGNALING_ADDR=0.0.0.0:9200 \
    P2P_STUN_ADDR=0.0.0.0:3478
USER app
CMD ["signaling"]

FROM debian:bookworm-slim AS relay

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && adduser --disabled-password --no-create-home --gecos "" app

COPY --from=builder /app/target/release/relay /usr/local/bin/relay
EXPOSE 9300 9092
ENV P2P_RELAY_ADDR=0.0.0.0:9300
USER app
CMD ["relay"]
