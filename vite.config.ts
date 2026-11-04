/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import pkg from "./package.json" with { type: "json" };

const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  plugins: [react(), tailwindcss()],
  clearScreen: false,
  // the About section's version (also in tests and the browser preview, where
  // Tauri's getVersion() is unavailable)
  define: { __APP_VERSION__: JSON.stringify(pkg.version) },
  // Only VITE_* variables reach the bundle; TP_* test credentials stay local.
  envPrefix: ["VITE_"],
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: "ws", host, port: 1421 } : undefined,
    watch: { ignored: ["**/src-tauri/**", "**/third_party/**", "**/.deps/**"] },
  },
  build: {
    target: "es2022",
    sourcemap: false,
    chunkSizeWarningLimit: 1500,
  },
  // only this checkout's tests: .deps/ holds gitignored copies of src/
  test: { include: ["src/**/*.test.{ts,tsx}"] },
});
