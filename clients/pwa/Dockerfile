# ── Build stage ────────────────────────────────────────────────────────
FROM node:25-alpine@sha256:ad82ecad30371c43f4057aaa4800a8ed88f9446553a2d21323710c7b937177fc AS build

WORKDIR /app/clients/pwa
COPY clients/pwa/package.json clients/pwa/package-lock.json* ./
RUN npm ci --ignore-scripts
COPY clients/desktop/src/index.css /app/clients/desktop/src/index.css
COPY clients/pwa/ .
RUN npm run build

# ── Serve stage ───────────────────────────────────────────────────────
# Note: Caddy alpine image requires root to bind port 80.
# Static files are read-only; Caddy itself has minimal attack surface.
FROM caddy:2-alpine@sha256:3b2a0196e0687279c14c27adff9fc6b44acfa318dbb97eaebe385bdf99e5364c

COPY --from=build /app/clients/pwa/dist /srv
COPY clients/pwa/Caddyfile /etc/caddy/Caddyfile

EXPOSE 80
