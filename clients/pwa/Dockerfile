# ── Build stage ────────────────────────────────────────────────────────
FROM node:20-alpine@sha256:f598378b5240225e6beab68fa9f356db1fb8efe55173e6d4d8153113bb8f333c AS build

WORKDIR /app/clients/pwa
COPY clients/pwa/package.json clients/pwa/package-lock.json* ./
RUN npm ci --ignore-scripts
COPY clients/desktop/src/index.css /app/clients/desktop/src/index.css
COPY clients/pwa/ .
RUN npm run build

# ── Serve stage ───────────────────────────────────────────────────────
FROM caddy:2-alpine@sha256:834468128c7696cec0ceea6172f7d692daf645ae51983ca76e39da54a97c570d

RUN setcap -r /usr/bin/caddy \
    && addgroup -S app \
    && adduser -S -D -H -G app app

COPY --from=build --chown=app:app /app/clients/pwa/dist /srv
COPY --chown=app:app clients/pwa/Caddyfile /etc/caddy/Caddyfile

ENV XDG_CONFIG_HOME=/tmp/caddy-config \
    XDG_DATA_HOME=/tmp/caddy-data

USER app
EXPOSE 8080
