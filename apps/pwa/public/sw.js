/// Hashed `/assets/*` are immutable and served cache-first; everything else is
/// network-first so fixes reach users on the next load.
const CACHE_NAME = "cypher-pwa-__BUILD_HASH__";
/** What another app shared, until the page takes it (`apps/pwa/src/share.ts`). */
const SHARE_CACHE = "cypher-share";

// A new version waits until the page asks for it: taking over mid-session
// would drop the old cache while the open page still loads chunks from it.
self.addEventListener("install", (e) => {
  e.waitUntil(caches.open(CACHE_NAME).then((cache) => cache.addAll(["/", "/index.html"])));
});

self.addEventListener("message", (e) => {
  if (e.data === "SKIP_WAITING") self.skipWaiting();
});

self.addEventListener("activate", (e) => {
  e.waitUntil(
    caches
      .keys()
      .then((keys) =>
        Promise.all(keys.filter((k) => k !== CACHE_NAME && k !== SHARE_CACHE).map((k) => caches.delete(k))),
      )
      .then(() => self.clients.claim()),
  );
});

/** Text of the wake-up notification; the worker knows nothing more. */
function wakeUp() {
  const ru = (self.navigator.language || "").toLowerCase().startsWith("ru");
  return ru ? ["Шифр", "Новое сообщение"] : ["Cypher", "New message"];
}

// The server signals only that the inbox has news. An open, visible app
// fetches it itself; otherwise the user learns there is something to read.
self.addEventListener("push", (e) => {
  e.waitUntil(
    self.clients.matchAll({ type: "window", includeUncontrolled: true }).then((clients) => {
      if (clients.some((c) => c.visibilityState === "visible")) return undefined;
      const [title, body] = wakeUp();
      return self.registration.showNotification(title, {
        body,
        tag: "messages",
        icon: "/icons/icon-192.png",
      });
    }),
  );
});

// The browser replaced the subscription: an open app registers it again
// (registration needs the identity, which only the app holds).
self.addEventListener("pushsubscriptionchange", (e) => {
  e.waitUntil(
    self.clients
      .matchAll({ type: "window", includeUncontrolled: true })
      .then((clients) => clients.forEach((c) => c.postMessage("PUSH_RENEW"))),
  );
});

self.addEventListener("notificationclick", (e) => {
  e.notification.close();
  e.waitUntil(
    self.clients.matchAll({ type: "window", includeUncontrolled: true }).then((clients) => {
      const open = clients.find((c) => "focus" in c);
      return open ? open.focus() : self.clients.openWindow("/");
    }),
  );
});

function store(request, response) {
  if (response.ok) {
    const copy = response.clone();
    caches.open(CACHE_NAME).then((cache) => cache.put(request, copy));
  }
  return response;
}

/** Keeps a share (files, text) and opens the app, which asks for a chat. */
async function keepShare(request) {
  try {
    const form = await request.formData();
    await caches.delete(SHARE_CACHE);
    const cache = await caches.open(SHARE_CACHE);
    const files = form.getAll("files").filter((f) => f instanceof File);
    await Promise.all(
      files.map((file, i) =>
        cache.put(
          `/share/file/${i}`,
          new Response(file, {
            headers: {
              "content-type": file.type || "application/octet-stream",
              "x-name": encodeURIComponent(file.name),
            },
          }),
        ),
      ),
    );
    const text = ["title", "text", "url"]
      .map((key) => form.get(key))
      .filter((v) => typeof v === "string" && v)
      .join("\n");
    await cache.put("/share/text", new Response(text));
  } catch (err) {
    console.warn("share target:", err);
  }
  return Response.redirect("/?share", 303);
}

self.addEventListener("fetch", (e) => {
  const { request } = e;
  const url = new URL(request.url);
  if (request.method === "POST" && url.origin === location.origin && url.pathname === "/share") {
    e.respondWith(keepShare(request));
    return;
  }
  if (request.method !== "GET" || url.origin !== location.origin) return;
  if (url.pathname.startsWith("/ws") || url.pathname.startsWith("/relay")) return;

  if (url.pathname.startsWith("/assets/")) {
    e.respondWith(caches.match(request).then((hit) => hit || fetch(request).then((r) => store(request, r))));
    return;
  }
  const fallback = request.mode === "navigate" ? "/index.html" : request;
  e.respondWith(
    fetch(request)
      .then((r) => store(request, r))
      .catch(() => caches.match(fallback)),
  );
});
