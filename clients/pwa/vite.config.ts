import { defineConfig } from "vite";
import solid from "vite-plugin-solid";
import { readFileSync, writeFileSync } from "fs";
import { resolve } from "path";
import { createHash } from "crypto";
import type { Plugin } from "vite";

/** Replace __BUILD_HASH__ in sw.js with a content hash after build. */
function swCacheVersion(): Plugin {
  return {
    name: "sw-cache-version",
    apply: "build",
    closeBundle() {
      const swPath = resolve(__dirname, "dist/sw.js");
      try {
        let content = readFileSync(swPath, "utf-8");
        const hash = createHash("md5").update(Date.now().toString()).digest("hex").slice(0, 8);
        content = content.replace("__BUILD_HASH__", hash);
        writeFileSync(swPath, content);
      } catch {
        // sw.js may not exist in dist if not copied
      }
    },
  };
}

export default defineConfig({
  plugins: [solid(), swCacheVersion()],
  build: { target: ["es2021", "chrome97", "safari15"] },
  clearScreen: false,
  server: { port: 5174, strictPort: true, host: "0.0.0.0" },
});
