import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

// `npm run dev` serves the page on :5173 and forwards the API to a
// gateway running with `[admin] bind = "127.0.0.1:9091"`. The Host header
// is passed through unchanged so login's same-origin check sees the
// page's own address.
const ADMIN = "http://127.0.0.1:9091";

export default defineConfig({
  plugins: [react()],
  server: {
    host: "127.0.0.1",
    port: 5173,
    strictPort: true,
    proxy: { "/admin/api": { target: ADMIN, changeOrigin: false } },
  },
  build: {
    outDir: "dist",
    // The page is served under a strict Content Security Policy: no
    // inline scripts, and no modules from anywhere but the admin port.
    modulePreload: { polyfill: false },
    assetsInlineLimit: 0,
  },
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./src/test/setup.ts"],
    restoreMocks: true,
  },
});
