import { describe, expect, it } from "vitest";
import { snippetSegments } from "./snippet";

describe("snippetSegments", () => {
  it("splits marks and decodes entities", () => {
    expect(snippetSegments("a <mark>cross</mark> <mark>of</mark> gold &amp; &lt;b&gt;")).toEqual([
      { text: "a ", mark: false },
      { text: "cross", mark: true },
      { text: " ", mark: false },
      { text: "of", mark: true },
      { text: " gold & <b>", mark: false },
    ]);
  });

  it("never turns other markup into elements", () => {
    const segs = snippetSegments("<img src=x onerror=alert(1)> <mark>&lt;script&gt;</mark>");
    expect(segs.every((s) => typeof s.text === "string")).toBe(true);
    expect(segs[0]!.text).toContain("<img");
    expect(segs[1]).toEqual({ text: "<script>", mark: true });
  });
});
