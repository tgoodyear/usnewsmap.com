/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// In development the API runs locally (`cargo run -p usnm-api`) and is
// proxied, so the app talks to a same-origin `/v1`. Production builds set
// VITE_API_BASE (e.g. https://api.usnewsmap.com).
const api = process.env.USNM_API_ORIGIN ?? "http://127.0.0.1:8080";

export default defineConfig({
  plugins: [react()],
  // MapLibre's worker is an ES module that imports a shared chunk.
  worker: { format: "es" },
  server: { proxy: { "/v1": api } },
  preview: { proxy: { "/v1": api } },
  build: {
    target: "es2022",
    sourcemap: true,
    // The map stack (MapLibre + deck.gl) is split out by the lazy import of
    // MapView, off the critical path (07 §7.7); that chunk is large by nature.
    chunkSizeWarningLimit: 2000,
  },
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.{ts,tsx}"],
  },
});
