import { readFileSync } from "node:fs";
import { defineConfig } from "vite";
import solid from "vite-plugin-solid";

const { version } = JSON.parse(readFileSync(new URL("package.json", import.meta.url), "utf-8")) as { version: string };

export default defineConfig({
  plugins: [solid()],
  build: { target: ["es2021", "chrome97"] },
  clearScreen: false,
  server: { port: 5173, strictPort: true },
  envPrefix: ["VITE_", "TAURI_ENV_*"],
  define: { __APP_VERSION__: JSON.stringify(version) },
});
