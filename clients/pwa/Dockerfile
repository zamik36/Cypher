# ── Build stage ────────────────────────────────────────────────────────
FROM node:20-alpine AS build

WORKDIR /app/clients/pwa
COPY clients/pwa/package.json clients/pwa/package-lock.json* ./
RUN npm ci --ignore-scripts
COPY clients/desktop/src/index.css /app/clients/desktop/src/index.css
COPY clients/pwa/ .
RUN npm run build

# ── Serve stage ───────────────────────────────────────────────────────
# Note: Caddy alpine image requires root to bind port 80.
# Static files are read-only; Caddy itself has minimal attack surface.
FROM caddy:2-alpine

COPY --from=build /app/clients/pwa/dist /srv
COPY clients/pwa/Caddyfile /etc/caddy/Caddyfile

EXPOSE 80
