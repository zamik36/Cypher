import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { describe, expect, it } from "vitest";

const ROOT = join(import.meta.dirname, "..", "..", "..", "..");
const SCANNED = ["packages/ui/src", "apps/pwa/src", "apps/desktop/src"];
/** Colours live only in the tokens. */
const TOKENS = "packages/ui/src/styles/tokens.css";
const LITERAL_COLOUR = /#[0-9a-f]{3,8}\b|\brgba?\(|\bhsla?\(|gradient\(/i;

function stylesheets(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) return name === "wasm" ? [] : stylesheets(path);
    return path.endsWith(".css") ? [path] : [];
  });
}

describe("stylesheets", () => {
  it("take every colour from the design tokens", () => {
    const offending = SCANNED.flatMap((dir) => stylesheets(join(ROOT, dir)))
      .map((path) => relative(ROOT, path).replaceAll("\\", "/"))
      .filter((path) => path !== TOKENS)
      .flatMap((path) =>
        readFileSync(join(ROOT, path), "utf-8")
          .split("\n")
          .map((line, i) => ({ path, line: i + 1, text: line.trim() }))
          .filter(({ text }) => LITERAL_COLOUR.test(text)),
      );
    expect(offending).toEqual([]);
  });
});
