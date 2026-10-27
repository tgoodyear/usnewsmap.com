// @vitest-environment node
import { afterEach, expect, it } from "vitest";
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { pageFingerprint } from "../sitemap";

const root = resolve(".sitemap-test-sources");
afterEach(() => rmSync(root, { recursive: true, force: true }));

it("tracks page copy and shared visible sources, not emitted chunk names or build metadata", () => {
  mkdirSync(resolve(root, "assets"), { recursive: true });
  writeFileSync(resolve(root, "page.tsx"), 'import { Brand } from "./brand"; export const page = <Brand />;');
  writeFileSync(resolve(root, "brand.tsx"), "export const Brand = () => <h1>US News Map</h1>;");
  writeFileSync(resolve(root, "index.html"), "<title>US News Map</title>");
  const first = pageFingerprint(root, "page.tsx");
  writeFileSync(resolve(root, "assets/chunk-new-hash.js"), "generated bundle");
  writeFileSync(resolve(root, "sitemap-pages.json"), '{"generated_at":"different build"}');
  expect(pageFingerprint(root, "page.tsx")).toBe(first);
  writeFileSync(resolve(root, "brand.tsx"), "// Not visible copy.\nexport const Brand=()=> <h1>US News Map</h1>;");
  expect(pageFingerprint(root, "page.tsx")).toBe(first);
  writeFileSync(resolve(root, "brand.tsx"), "export const Brand = () => <h1>New shared copy</h1>;");
  expect(pageFingerprint(root, "page.tsx")).not.toBe(first);
  const shared = pageFingerprint(root, "page.tsx");
  writeFileSync(
    resolve(root, "page.tsx"),
    'import { Brand } from "./brand"; export const page = <><Brand /><p>New page copy</p></>;',
  );
  expect(pageFingerprint(root, "page.tsx")).not.toBe(shared);
});
