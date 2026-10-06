import solid from "vite-plugin-solid";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [solid({ hot: false })],
  define: { __APP_VERSION__: JSON.stringify("0.0.0-test") },
  test: {
    environment: "jsdom",
    include: ["packages/*/src/**/*.test.{ts,tsx}", "apps/*/src/**/*.test.{ts,tsx}"],
    restoreMocks: true,
    unstubGlobals: true,
    coverage: {
      provider: "v8",
      // Logic modules and the components with tests of their own; screens and
      // recorders are covered end to end by Playwright.
      include: [
        "packages/ui/src/components/ActionMenu.tsx",
        "packages/ui/src/components/FileCard.tsx",
        "packages/ui/src/components/ImageViewer.tsx",
        "packages/ui/src/stores/**",
        "packages/ui/src/utils/**",
        "packages/ui/src/i18n/**",
        "packages/ui/src/components/media/player.ts",
        "apps/pwa/src/web/effects.ts",
        "apps/pwa/src/web/idb.ts",
      ],
      exclude: ["**/*.test.*"],
      // A ratchet like `.config/coverage.toml`: measured minus one; raise, never lower.
      thresholds: { lines: 99, functions: 96, branches: 94, statements: 97 },
      reporter: ["text", "html"],
      reportsDirectory: "target/coverage/web",
    },
  },
});
