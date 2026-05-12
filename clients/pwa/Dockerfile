# ── Build stage ────────────────────────────────────────────────────────
FROM node:26-alpine@sha256:e71ac5e964b9201072425d59d2e876359efa25dc96bb1768cb73295728d6e4ea AS build

WORKDIR /app/clients/pwa
COPY clients/pwa/package.json clients/pwa/package-lock.json* ./
RUN npm ci --ignore-scripts
COPY clients/desktop/src/index.css /app/clients/desktop/src/index.css
COPY clients/pwa/ .
RUN npm run build

# ── Serve stage ───────────────────────────────────────────────────────
FROM caddy:2-alpine@sha256:fce4f15aad23222c0ac78a1220adf63bae7b94355d5ea28eee53910624acedfa

RUN setcap -r /usr/bin/caddy \
    && addgroup -S app \
    && adduser -S -D -H -G app app

COPY --from=build --chown=app:app /app/clients/pwa/dist /srv
COPY --chown=app:app clients/pwa/Caddyfile /etc/caddy/Caddyfile

ENV XDG_CONFIG_HOME=/tmp/caddy-config \
    XDG_DATA_HOME=/tmp/caddy-data

USER app
EXPOSE 8080
