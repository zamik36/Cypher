FROM rust:1.98-bookworm@sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e AS wasm

RUN rustup target add wasm32-unknown-unknown \
    && cargo install wasm-bindgen-cli --version 0.2.129 --locked

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
COPY services/ services/
COPY tools/ tools/
COPY tests/ tests/
COPY apps/desktop/src-tauri/ apps/desktop/src-tauri/
RUN cargo build -p cypher-wasm --release --target wasm32-unknown-unknown \
    && wasm-bindgen --target web --out-dir /wasm target/wasm32-unknown-unknown/release/cypher_wasm.wasm

FROM node:22-alpine@sha256:0a7108bf6c7bf5de370ffb1a3ed6be93d405b43ff159f681a8d18c0e2bc2e402 AS build

WORKDIR /app
COPY package.json package-lock.json tsconfig.base.json ./
COPY packages/ui/package.json packages/ui/
COPY apps/desktop/package.json apps/desktop/
COPY apps/pwa/package.json apps/pwa/
COPY tests/load/package.json tests/load/
RUN npm ci --ignore-scripts
COPY packages/ui/ packages/ui/
COPY apps/pwa/ apps/pwa/
COPY --from=wasm /wasm apps/pwa/src/wasm
RUN npm run build -w apps/pwa

FROM caddy:2-alpine@sha256:fce4f15aad23222c0ac78a1220adf63bae7b94355d5ea28eee53910624acedfa

RUN setcap -r /usr/bin/caddy \
    && addgroup -S app \
    && adduser -S -D -H -G app app

COPY --from=build --chown=app:app /app/apps/pwa/dist /srv
COPY --chown=app:app apps/pwa/Caddyfile /etc/caddy/Caddyfile

ENV XDG_CONFIG_HOME=/tmp/caddy-config \
    XDG_DATA_HOME=/tmp/caddy-data

USER app
EXPOSE 8080
