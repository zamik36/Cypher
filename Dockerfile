FROM rust:1.94-bookworm@sha256:6ae102bdbf528294bc79ad6e1fae682f6f7c2a6e6621506ba959f9685b308a55 AS builder

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

FROM debian:bookworm-slim@sha256:4724b8cc51e33e398f0e2e15e18d5ec2851ff0c2280647e1310bc1642182655d AS runtime-base

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && adduser --disabled-password --no-create-home --gecos "" app \
    && rm -rf /var/lib/apt/lists/* /var/cache/apt/* /var/log/* \
    && rm -rf /usr/share/doc /usr/share/man /usr/share/info \
    && rm -rf /etc/init.d /etc/rc0.d /etc/rc1.d /etc/rc2.d /etc/rc3.d /etc/rc4.d /etc/rc5.d /etc/rc6.d /etc/rcS.d \
    && rm -rf /etc/apt /var/lib/apt /var/lib/dpkg \
    && rm -f /usr/bin/apt /usr/bin/apt-* /usr/bin/dpkg /usr/bin/dpkg-*

FROM runtime-base AS gateway

COPY --from=builder /app/target/release/gateway /usr/local/bin/gateway
EXPOSE 9100 9101 9090
ENV P2P_GATEWAY_ADDR=0.0.0.0:9100 \
    P2P_WS_ADDR=0.0.0.0:9101
USER app
CMD ["gateway"]

FROM runtime-base AS signaling

COPY --from=builder /app/target/release/signaling /usr/local/bin/signaling
RUN mkdir -p /data/signaling && chown app:app /data/signaling
EXPOSE 9200 3478/udp 9091
ENV P2P_SIGNALING_ADDR=0.0.0.0:9200 \
    P2P_STUN_ADDR=0.0.0.0:3478
WORKDIR /
USER app
CMD ["signaling"]

FROM runtime-base AS relay

COPY --from=builder /app/target/release/relay /usr/local/bin/relay
RUN mkdir -p /data/relay && chown app:app /data/relay
EXPOSE 9300 9092
WORKDIR /
USER app
CMD ["relay"]
