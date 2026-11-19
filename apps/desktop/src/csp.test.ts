import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

const app = join(import.meta.dirname, "..");
const read = (path: string) => readFileSync(join(app, path), "utf-8");

/** The policy Tauri injects into every page it serves. */
function policy(): Map<string, string[]> {
  const conf = JSON.parse(read("src-tauri/tauri.conf.json")) as { app: { security: { csp: string } } };
  return new Map(
    conf.app.security.csp
      .split(";")
      .map((d) => d.trim().split(/\s+/))
      .filter((d) => d[0])
      .map(([name = "", ...sources]) => [name, sources]),
  );
}

describe("content security policy", () => {
  it("has one source: a meta tag would be enforced on top and block media", () => {
    expect(read("index.html")).not.toMatch(/Content-Security-Policy/i);
  });

  it("lets voice and video notes load from the media scheme, and posters from blobs", () => {
    const csp = policy();
    expect(csp.get("media-src")).toEqual(expect.arrayContaining(["cypher-media:", "http://cypher-media.localhost"]));
    expect(csp.get("img-src")).toContain("blob:");
    expect(csp.get("script-src")).toEqual(["'self'"]);
  });
});
