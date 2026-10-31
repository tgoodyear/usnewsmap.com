import { describe, expect, it, vi } from "vitest";

import { reportEdgeExample } from "./edgeExamples";

const response = (headers: Record<string, string>) => ({ headers: new Headers(headers) });

describe("reportEdgeExample", () => {
  it("reports an example the edge answered, once per page load", () => {
    const sendBeacon = vi.fn(() => true);
    const hit = response({ "x-usnm-example": "whigs", "cf-cache-status": "HIT" });
    reportEdgeExample(hit, { sendBeacon });
    reportEdgeExample(hit, { sendBeacon });
    expect(sendBeacon).toHaveBeenCalledTimes(1);
    expect(sendBeacon).toHaveBeenCalledWith(expect.stringMatching(/\/v1\/example-search$/), '{"example":"whigs"}');
  });

  it("leaves what reached the API, and other searches, alone", () => {
    const sendBeacon = vi.fn(() => true);
    reportEdgeExample(response({ "x-usnm-example": "gold", "cf-cache-status": "MISS" }), { sendBeacon });
    reportEdgeExample(response({ "x-usnm-example": "silver" }), { sendBeacon });
    reportEdgeExample(response({ "cf-cache-status": "HIT" }), { sendBeacon });
    expect(sendBeacon).not.toHaveBeenCalled();
  });
});
