/// Service Worker for Cypher PWA — cache-first for static assets.
/// __BUILD_HASH__ is replaced at build time by vite; falls back to "v2" in dev.
const CACHE_NAME = "cypher-pwa-__BUILD_HASH__";
const PRECACHE = ["/", "/index.html"];

self.addEventListener("install", (e) => {
  e.waitUntil(
    caches.open(CACHE_NAME).then((cache) => cache.addAll(PRECACHE))
  );
  self.skipWaiting();
});

self.addEventListener("activate", (e) => {
  e.waitUntil(
    caches.keys().then((keys) =>
      Promise.all(keys.filter((k) => k !== CACHE_NAME).map((k) => caches.delete(k)))
    )
  );
  self.clients.claim();
});

self.addEventListener("fetch", (e) => {
  const { request } = e;
  // Skip non-GET and WebSocket requests.
  if (request.method !== "GET" || request.url.includes("/ws")) return;

  // Navigation fallback: return index.html for SPA routes.
  if (request.mode === "navigate") {
    e.respondWith(
      fetch(request)
        .then((response) => {
          if (response.ok) {
            const clone = response.clone();
            caches.open(CACHE_NAME).then((cache) => cache.put(request, clone));
          }
          return response;
        })
        .catch(() => caches.match("/index.html").then((r) => r || fetch(request)))
    );
    return;
  }

  // Static assets: network-first — ensures security patches reach users quickly.
  // Falls back to cache only when offline.
  e.respondWith(
    fetch(request)
      .then((response) => {
        if (response.ok) {
          const clone = response.clone();
          caches.open(CACHE_NAME).then((cache) => cache.put(request, clone));
        }
        return response;
      })
      .catch(() => caches.match(request))
  );
});
