/// Service Worker for P2P Share PWA — cache-first for static assets.
const CACHE_NAME = "p2p-pwa-v1";
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

  e.respondWith(
    caches.match(request).then((cached) => {
      // Return cached, but also fetch and update cache in background.
      const fetched = fetch(request)
        .then((response) => {
          if (response.ok) {
            const clone = response.clone();
            caches.open(CACHE_NAME).then((cache) => cache.put(request, clone));
          }
          return response;
        })
        .catch(() => cached);
      return cached || fetched;
    })
  );
});
