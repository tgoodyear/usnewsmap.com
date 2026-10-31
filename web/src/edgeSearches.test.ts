import { describe, expect, it, vi } from "vitest";

import { reportEdgeSearch } from "./edgeSearches";

const response = (query: string, status?: string) => ({
  url: `https://usnewsmap.com/v1/aggregate?${query}`,
  headers: new Headers(status ? { "cf-cache-status": status } : {}),
});

describe("reportEdgeSearch", () => {
  it("reports a search the edge answered, once per page load", () => {
    const sendBeacon = vi.fn(() => true);
    const hit = response("q=gold&v=v1&g=f8", "HIT");
    reportEdgeSearch(hit, { sendBeacon });
    reportEdgeSearch(hit, { sendBeacon });
    expect(sendBeacon).toHaveBeenCalledTimes(1);
    expect(sendBeacon).toHaveBeenCalledWith(
      expect.stringMatching(/\/v1\/search-report$/),
      '{"query":"q=gold&v=v1&g=f8"}',
    );
  });

  it("sends nothing for a visitor who opted out", () => {
    const sendBeacon = vi.fn(() => true);
    reportEdgeSearch(response("q=copper&v=v1&g=f8", "HIT"), { sendBeacon, globalPrivacyControl: true });
    reportEdgeSearch(response("q=copper&v=v1&g=f8", "HIT"), { sendBeacon, doNotTrack: "1" });
    expect(sendBeacon).not.toHaveBeenCalled();
  });

  it("leaves what reached the API alone", () => {
    const sendBeacon = vi.fn(() => true);
    reportEdgeSearch(response("q=silver&v=v1&g=f8", "MISS"), { sendBeacon });
    reportEdgeSearch(response("q=silver&v=v1&g=f8", "DYNAMIC"), { sendBeacon });
    reportEdgeSearch(response("q=silver&v=v1&g=f8"), { sendBeacon });
    expect(sendBeacon).not.toHaveBeenCalled();
  });
});
