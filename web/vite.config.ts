/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { parse } from "smol-toml";
import examples from "./src/examples.json" with { type: "json" };
import { searchPath } from "./src/route";

// In development the API runs locally (`cargo run -p usnm-api`) and is
// proxied, so the app talks to a same-origin `/v1`. Production builds set
// VITE_API_BASE (e.g. https://api.usnewsmap.com).
const api = process.env.USNM_API_ORIGIN ?? "http://127.0.0.1:8080";

export default defineConfig({
  plugins: [
    react(),
    {
      name: "ocr-credit-catalog",
      transform(text, id) {
        if (!id.endsWith("/ocr/engines/engines.toml")) return;
        return { code: `export default ${JSON.stringify(parse(text))}`, map: null };
      },
    },
    {
      name: "search-sitemap",
      generateBundle() {
        const paths = ["/", "/privacy", ...new Set(examples.map(({ view }) => searchPath(view.q)))];
        const source =
          '<?xml version="1.0" encoding="UTF-8"?>\n' +
          '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n' +
          paths.map((path) => `  <url><loc>https://usnewsmap.com${path}</loc></url>`).join("\n") +
          "\n</urlset>\n";
        this.emitFile({ type: "asset", fileName: "sitemap.xml", source });
      },
    },
  ],
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
    // The status page writes times in the visitor's zone; tests pin one.
    env: { TZ: "America/New_York" },
  },
});
