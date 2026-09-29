import solid from "vite-plugin-solid";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [solid()],
  test: {
    environment: "jsdom",
    include: ["packages/*/src/**/*.test.{ts,tsx}", "apps/*/src/**/*.test.{ts,tsx}"],
    restoreMocks: true,
    unstubGlobals: true,
    coverage: {
      provider: "v8",
      // Logic modules; components and recorders are covered end to end by Playwright.
      include: [
        "packages/ui/src/stores/**",
        "packages/ui/src/utils/**",
        "packages/ui/src/i18n/**",
        "packages/ui/src/components/media/player.ts",
        "apps/pwa/src/web/effects.ts",
        "apps/pwa/src/web/idb.ts",
      ],
      exclude: ["**/*.test.*"],
      // A ratchet like `.config/coverage.toml`: measured minus one; raise, never lower.
      thresholds: { lines: 99, functions: 93, branches: 94, statements: 97 },
      reporter: ["text", "html"],
      reportsDirectory: "target/coverage/web",
    },
  },
});
