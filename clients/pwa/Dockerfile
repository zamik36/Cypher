# ── Build stage ────────────────────────────────────────────────────────
FROM node:20-alpine@sha256:f598378b5240225e6beab68fa9f356db1fb8efe55173e6d4d8153113bb8f333c AS build

WORKDIR /app/clients/pwa
COPY clients/pwa/package.json clients/pwa/package-lock.json* ./
RUN npm ci --ignore-scripts
COPY clients/desktop/src/index.css /app/clients/desktop/src/index.css
COPY clients/pwa/ .
RUN npm run build

# ── Serve stage ───────────────────────────────────────────────────────
# Note: Caddy alpine image requires root to bind port 80.
# Static files are read-only; Caddy itself has minimal attack surface.
FROM caddy:2-alpine@sha256:fce4f15aad23222c0ac78a1220adf63bae7b94355d5ea28eee53910624acedfa

COPY --from=build /app/clients/pwa/dist /srv
COPY clients/pwa/Caddyfile /etc/caddy/Caddyfile

EXPOSE 80
