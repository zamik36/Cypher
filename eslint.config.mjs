// @ts-check
import js from "@eslint/js";
import prettier from "eslint-config-prettier/flat";
import { defineConfig } from "eslint/config";
import solid from "eslint-plugin-solid/configs/typescript";
import globals from "globals";
import tseslint from "typescript-eslint";

export default defineConfig(
  {
    ignores: ["**/dist/", "**/node_modules/", "**/target/", "apps/pwa/src/wasm/", "apps/desktop/src-tauri/"],
  },
  {
    files: ["**/*.{ts,tsx}"],
    extends: [js.configs.recommended, tseslint.configs.strictTypeChecked, tseslint.configs.stylisticTypeChecked, solid],
    languageOptions: {
      parserOptions: {
        projectService: true,
        tsconfigRootDir: import.meta.dirname,
      },
    },
    rules: {
      // `onClick={() => setOpen(false)}` is the Solid idiom, not a confusing void.
      "@typescript-eslint/no-confusing-void-expression": ["error", { ignoreArrowShorthand: true }],
      // Async JSX event handlers are fine; every other void-returning slot is still checked.
      "@typescript-eslint/no-misused-promises": ["error", { checksVoidReturn: { attributes: false } }],
      "@typescript-eslint/restrict-template-expressions": ["error", { allowNumber: true }],
      // `name || fallback` deliberately treats "" as missing.
      "@typescript-eslint/prefer-nullish-coalescing": ["error", { ignorePrimitives: { string: true } }],
    },
  },
  {
    // Solid compiles `ref={el}` into an assignment the rule cannot see.
    files: ["**/*.tsx"],
    rules: { "no-unassigned-vars": "off" },
  },
  {
    // Build configs live outside the apps' browser tsconfigs.
    files: ["apps/*/vite.config.ts", "vitest.config.ts"],
    languageOptions: { parserOptions: { projectService: false, project: "./tsconfig.node.json" } },
  },
  {
    files: ["**/*.{js,mjs}"],
    extends: [js.configs.recommended],
    languageOptions: { globals: globals.node },
  },
  {
    files: ["apps/pwa/public/sw.js"],
    languageOptions: { globals: globals.serviceworker },
  },
  {
    // k6 scenarios run in k6's runtime, not Node.
    files: ["tests/load/src/**/*.js"],
    ignores: ["tests/load/src/**/*.test.js"],
    languageOptions: { globals: { __ENV: "readonly", __VU: "readonly", __ITER: "readonly" } },
  },
  prettier,
);
