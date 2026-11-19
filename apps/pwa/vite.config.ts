import { randomBytes } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { defineConfig, type Plugin } from "vite";
import solid from "vite-plugin-solid";

/** Gives every build its own service-worker cache name. */
function swCacheVersion(): Plugin {
  return {
    name: "sw-cache-version",
    apply: "build",
    closeBundle() {
      const path = resolve(import.meta.dirname, "dist/sw.js");
      const build = randomBytes(4).toString("hex");
      writeFileSync(path, readFileSync(path, "utf-8").replace("__BUILD_HASH__", build));
    },
  };
}

const { version } = JSON.parse(readFileSync(resolve(import.meta.dirname, "package.json"), "utf-8")) as {
  version: string;
};

const gatewayWs = process.env["CYPHER_DEV_GATEWAY_WS"] ?? "ws://127.0.0.1:9101";
const relayWs = process.env["CYPHER_DEV_RELAY_WS"] ?? "ws://127.0.0.1:9301";
/** The client always talks to its own origin; dev and preview forward to a local stack. */
const proxy = {
  "/ws": { target: gatewayWs, ws: true },
  "/relay": { target: relayWs, ws: true },
};

export default defineConfig({
  plugins: [solid(), swCacheVersion()],
  build: { target: ["es2022", "chrome102", "safari16"] },
  worker: { format: "es" },
  define: { __APP_VERSION__: JSON.stringify(version) },
  clearScreen: false,
  server: { port: 5174, strictPort: true, proxy },
  preview: { port: 4173, strictPort: true, proxy },
});
