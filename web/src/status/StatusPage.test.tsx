import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import StatusPage from "./StatusPage";

const unavailable = { available: false, reason: "This server is not connected to the pipeline state." };

const body = {
  schema: 1,
  generated_at: new Date().toISOString(),
  stale: false,
  error: null,
  pipeline: { available: false, read_at: null, reason: unavailable.reason },
  published: {
    index_version: "fixture-v1",
    published_at: "2026-09-27T00:00:00Z",
    synthetic: true,
    pages: 1872,
    titles: 6,
    places: 6,
    bounds: { from: "1895-01-01", to: "1897-12-31" },
    indexes: ["pages-base-fixture", "pages-delta-fixture-1"],
    deltas: 1,
    max_deltas: 8,
    next_release_full: false,
    batches: null,
  },
  backfill: unavailable,
  indexing: unavailable,
  titles: { catalog: { available: false, reason: "none" }, published_titles: 6, published_places: 6, pipeline: unavailable },
};

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("StatusPage", () => {
  it("shows the published version and says which sections aren't available", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response(JSON.stringify(body), { headers: { "content-type": "application/json" } })),
    );
    render(
      <QueryClientProvider client={new QueryClient()}>
        <StatusPage />
      </QueryClientProvider>,
    );
    expect(await screen.findByRole("heading", { level: 1, name: "Pipeline status" })).toBeTruthy();
    expect(await screen.findByText("fixture-v1")).toBeTruthy();
    expect(screen.getAllByText(/^Not available\./)).toHaveLength(3);
    expect(screen.getByText(/1 of 8 deltas used/)).toBeTruthy();
    expect(screen.getByRole("link", { name: "/v1/status" }).getAttribute("href")).toBe("/v1/status");
    expect(vi.mocked(fetch).mock.calls[0]![0]).toBe("/v1/status");
  });
});
