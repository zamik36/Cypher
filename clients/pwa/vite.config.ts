import { defineConfig } from "vite";
import solid from "vite-plugin-solid";

export default defineConfig({
  plugins: [solid()],
  build: { target: ["es2021", "chrome97", "safari15"] },
  clearScreen: false,
  server: { port: 5174, strictPort: true, host: "0.0.0.0" },
});
