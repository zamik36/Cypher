# ── Build stage ────────────────────────────────────────────────────────
FROM node:20-alpine AS build

WORKDIR /app
COPY package.json package-lock.json* ./
RUN npm ci --ignore-scripts
COPY . .
RUN npm run build

# ── Serve stage ───────────────────────────────────────────────────────
# Note: Caddy alpine image requires root to bind port 80.
# Static files are read-only; Caddy itself has minimal attack surface.
FROM caddy:2-alpine

COPY --from=build /app/dist /srv
COPY Caddyfile /etc/caddy/Caddyfile

EXPOSE 80
