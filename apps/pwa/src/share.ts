import type { Shared } from "@cypher/ui/share";

/** Where the service worker keeps a share (`public/sw.js`). */
const SHARE_CACHE = "cypher-share";
const FILE_PREFIX = "/share/file/";

/**
 * Takes what the service worker kept from a share and forgets it, so the
 * files stay on disk only until the app opens.
 */
export async function takeShared(): Promise<Shared | null> {
  if (!("caches" in window) || !(await caches.has(SHARE_CACHE))) return null;
  const cache = await caches.open(SHARE_CACHE);
  try {
    const requests = await cache.keys();
    const index = (r: Request) => Number(new URL(r.url).pathname.slice(FILE_PREFIX.length));
    const parts = requests.filter((r) => new URL(r.url).pathname.startsWith(FILE_PREFIX));
    parts.sort((a, b) => index(a) - index(b));
    const files = await Promise.all(
      parts.map(async (request) => {
        const response = await cache.match(request);
        const blob = (await response?.blob()) ?? new Blob();
        const name = decodeURIComponent(response?.headers.get("x-name") ?? "file");
        return new File([blob], name, { type: blob.type });
      }),
    );
    const text = (await (await cache.match("/share/text"))?.text()) ?? "";
    return { files, text };
  } finally {
    await caches.delete(SHARE_CACHE);
  }
}
