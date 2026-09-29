import { describe, expect, it } from "vitest";
import { pageFor } from "./route";

describe("pageFor", () => {
  it("knows the app's pages, with or without a trailing slash", () => {
    expect(pageFor("/")).toBe("search");
    expect(pageFor("//")).toBe("search");
    expect(pageFor("/status")).toBe("status");
    expect(pageFor("/status/")).toBe("status");
  });

  it("finds nothing anywhere else", () => {
    for (const path of ["/does-not-exist", "/statuses", "/status/x", "/search/gold"]) {
      expect(pageFor(path)).toBe("not-found");
    }
  });
});
