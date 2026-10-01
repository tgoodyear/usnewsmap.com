import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import StatusPage, { ErrorText } from "./StatusPage";

const unavailable = {
  available: false,
  reason: "This server is not connected to the pipeline state.",
};

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
  titles: {
    catalog: { available: false, reason: "none" },
    published_titles: 6,
    published_places: 6,
    pipeline: unavailable,
  },
};

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("StatusPage", () => {
  it("shows the published version and says which sections aren't available", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(
        async () =>
          new Response(JSON.stringify(body), {
            headers: { "content-type": "application/json" },
          }),
      ),
    );
    render(
      <QueryClientProvider client={new QueryClient()}>
        <StatusPage />
      </QueryClientProvider>,
    );
    expect(
      await screen.findByRole("heading", { level: 1, name: "Pipeline status" }),
    ).toBeTruthy();
    expect(await screen.findByText("fixture-v1")).toBeTruthy();
    expect(screen.getAllByText(/^Not available\./)).toHaveLength(3);
    expect(screen.getByText(/1 of 8 deltas used/)).toBeTruthy();
    expect(
      screen.getByRole("link", { name: "/v1/status" }).getAttribute("href"),
    ).toBe("/v1/status");
    expect(vi.mocked(fetch).mock.calls[0]![0]).toBe("/v1/status");
  });

  it("shows the pages tables when the API sends them, and leaves them out when it doesn't", async () => {
    const withTables = {
      ...body,
      published: {
        ...body.published,
        by_state: [
          {
            state: "IL",
            name: "Illinois",
            places: 1,
            titles: 1,
            pages: 1872,
            percent: 100,
          },
        ],
        by_language: {
          pages_known: true,
          multilingual_titles: 0,
          multilingual_pages: 0,
          rows: [
            {
              code: "eng",
              name: "English",
              titles: 6,
              pages: 1872,
              percent: 100,
            },
          ],
        },
      },
    };
    for (const [doc, shown] of [
      [withTables, true],
      [body, false],
    ] as const) {
      vi.stubGlobal(
        "fetch",
        vi.fn(
          async () =>
            new Response(JSON.stringify(doc), {
              headers: { "content-type": "application/json" },
            }),
        ),
      );
      render(
        <QueryClientProvider client={new QueryClient()}>
          <StatusPage />
        </QueryClientProvider>,
      );
      expect(await screen.findByText("fixture-v1")).toBeTruthy();
      for (const name of ["Pages by state", "Pages by language"]) {
        expect(screen.queryByRole("heading", { level: 2, name }) !== null).toBe(
          shown,
        );
      }
      cleanup();
    }
  });

  it("shows a short error in full and a long one as a closed preview", () => {
    const short = "x".repeat(60);
    const long = `quickwit returned 503: ${"y".repeat(300)}`;
    const { container } = render(
      <>
        <ErrorText text={short} />
        <ErrorText text={long} />
        <ErrorText text={null} />
      </>,
    );
    // 60 characters: in full, with nothing to expand.
    expect(screen.getByText(short).tagName).toBe("SPAN");
    // Longer: a closed <details> whose summary is the first 60 characters.
    const details = container.querySelector("details.status-error")!;
    expect(details).toBeTruthy();
    expect(details.hasAttribute("open")).toBe(false);
    expect(details.querySelector("summary")!.textContent).toBe(
      `${long.slice(0, 60)}…`,
    );
    expect(details.textContent).toContain(long);
    expect(container.querySelectorAll("details")).toHaveLength(1);
    // No error: a dash.
    expect(container.textContent!.endsWith("–")).toBe(true);
  });
});
