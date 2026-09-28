// @vitest-environment node
// Checks public/staticwebapp.config.json against the rules Azure Static Web
// Apps enforces at deploy time, so a bad rule fails here rather than in the
// deploy (the published JSON schema misses most of them). Sources:
// https://learn.microsoft.com/azure/static-web-apps/configuration, plus the
// deploy client's own errors noted inline.
import { readFileSync, existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const publicDir = fileURLToPath(new URL("../public/", import.meta.url));
const raw = readFileSync(`${publicDir}staticwebapp.config.json`, "utf8");

interface Route {
  route: string;
  methods?: string[];
  rewrite?: string;
  redirect?: string;
  statusCode?: number;
  headers?: Record<string, string>;
  allowedRoles?: string[];
}

const config = JSON.parse(raw) as {
  routes?: Route[];
  navigationFallback?: { rewrite: string; exclude?: string[] };
  responseOverrides?: Record<string, unknown>;
  [key: string]: unknown;
};
const routes = config.routes ?? [];

// A path the site serves: a file in public/ or the built index.html.
function served(path: string): boolean {
  return path === "/index.html" || existsSync(`${publicDir}${path.replace(/^\//, "")}`);
}

describe("staticwebapp.config.json", () => {
  it("is within the 20 KB limit", () => {
    expect(Buffer.byteLength(raw)).toBeLessThanOrEqual(20 * 1024);
  });

  it("uses only known sections", () => {
    const known = [
      "routes", "navigationFallback", "responseOverrides", "globalHeaders", "mimeTypes",
      "platform", "networking", "auth", "forwardingGateway", "trailingSlash",
    ];
    expect(Object.keys(config).filter((k) => !known.includes(k))).toEqual([]);
  });

  it.each(routes.map((r) => [r.route, r] as const))("route %s is valid", (_, r) => {
    const known = ["route", "methods", "rewrite", "redirect", "statusCode", "headers", "allowedRoles"];
    expect(Object.keys(r).filter((k) => !known.includes(k))).toEqual([]);
    expect(r.route.startsWith("/")).toBe(true);
    // rewrite and redirect are mutually exclusive.
    expect(r.rewrite !== undefined && r.redirect !== undefined).toBe(false);
    // Deploy error: "Status code cannot be specified for a rule with Rewrite specified."
    expect(r.rewrite !== undefined && r.statusCode !== undefined).toBe(false);
    if (r.redirect !== undefined && r.statusCode !== undefined) {
      expect([301, 302]).toContain(r.statusCode);
    }
    if (r.rewrite !== undefined) expect(served(r.rewrite)).toBe(true);
  });

  it("excludes status-only routes from the navigation fallback", () => {
    // Route rules don't apply to requests the fallback serves, so a path
    // with no file behind it would get index.html instead of its status.
    const exclude = config.navigationFallback?.exclude ?? [];
    const statusOnly = routes.filter((r) => r.statusCode !== undefined && r.redirect === undefined);
    expect(statusOnly.map((r) => r.route).filter((p) => !exclude.includes(p))).toEqual([]);
  });

  it("falls back to a file the site serves", () => {
    if (config.navigationFallback) expect(served(config.navigationFallback.rewrite)).toBe(true);
  });

  it("overrides only the status codes Static Web Apps allows", () => {
    const codes = Object.keys(config.responseOverrides ?? {});
    expect(codes.filter((c) => !["400", "401", "403", "404"].includes(c))).toEqual([]);
  });
});
