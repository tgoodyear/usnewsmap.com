import { describe, expect, it } from "vitest";
import type { Status } from "../api/types";
import { health, relative, share, span } from "./format";

const NOW = Date.parse("2026-09-29T12:00:00Z");

function status(patch: Partial<Status> = {}): Status {
  return {
    schema: 1,
    generated_at: "2026-09-29T12:00:00Z",
    stale: false,
    error: null,
    pipeline: { available: false, read_at: null, reason: "not connected" },
    published: {
      index_version: "v1",
      published_at: "2026-09-29T10:00:00Z",
      synthetic: false,
      pages: 10,
      titles: 1,
      places: 1,
      bounds: { from: "1890-01-01", to: "1900-01-01" },
      indexes: ["b"],
      deltas: 0,
      max_deltas: 8,
      next_release_full: false,
      batches: null,
    },
    backfill: { available: false, reason: "not connected" },
    indexing: { available: false, reason: "not connected" },
    titles: {
      catalog: { available: false, reason: "no catalog" },
      published_titles: 1,
      published_places: 1,
      pipeline: { available: false, reason: "not connected" },
    },
    ...patch,
  };
}

const backfill = {
  available: true as const,
  total: 10,
  by_status: { queued: 3, downloading: 2, curated: 5, failed: 0 },
  in_progress: 2,
  stale_leases: 0,
  retrying: 0,
  percent: 50,
  pages: 100,
  ok_pages: 90,
  versions: { "01": 10 },
  newer_versions_pending: 0,
  throughput: {
    hours: [],
    rate_window_hours: 12,
    rate_per_hour: 1,
    remaining: 5,
    eta: null,
  },
  loc: { next_slot: null, blocked_until: null, throttled: false },
  in_progress_batches: [],
  recent: [],
  failed_batches: [],
  listed_limit: 100,
};

describe("status formatting", () => {
  it("gives spans in their two coarsest units", () => {
    expect(span(45_000)).toBe("45 s");
    expect(span(12 * 60_000)).toBe("12 min");
    expect(span((3 * 60 + 5) * 60_000)).toBe("3 h 5 min");
    expect(span(52 * 3_600_000)).toBe("2 d 4 h");
    expect(relative("2026-09-29T10:00:00Z", NOW)).toBe("2 h ago");
    expect(relative("2026-09-29T12:40:00Z", NOW)).toBe("in 40 min");
  });

  it("says when the pipeline can't be seen", () => {
    const h = health(status(), NOW);
    expect(h.level).toBe("unknown");
    expect(h.parts).toEqual([
      "Pipeline state not available",
      "last release 2 h ago",
    ]);
  });

  it("summarizes a running backfill", () => {
    const h = health(status({ backfill }), NOW);
    expect(h).toEqual({
      level: "ok",
      parts: [
        "Backfill running (2 in progress)",
        "last release 2 h ago",
        "no failures",
      ],
    });
  });

  it("flags failures, throttling and stale data", () => {
    const b = {
      ...backfill,
      by_status: { ...backfill.by_status, failed: 2 },
      loc: {
        next_slot: null,
        blocked_until: "2026-09-29T12:40:00Z",
        throttled: true,
      },
    };
    const h = health(status({ backfill: b }), NOW);
    expect(h.level).toBe("attention");
    expect(h.parts[0]).toMatch(/^Downloads paused by LoC until /);
    expect(h.parts.at(-1)).toBe("2 failures");
    expect(health(status({ backfill, stale: true }), NOW).level).toBe(
      "problem",
    );
  });

  it("doesn't count index runs a later publish superseded", () => {
    const indexing = {
      available: true as const,
      current_version: "v5",
      writer: { held: false, holder: null, until: null },
      release: null,
      last_published_at: "2026-09-29T10:00:00Z",
      failed_runs: 2,
      failed_since_last_publish: 0,
      runs: [],
    };
    const h = health(status({ backfill, indexing }), NOW);
    expect(h.level).toBe("ok");
    expect(h.parts.at(-1)).toBe("no failures");
    const unresolved = { ...indexing, failed_since_last_publish: 1 };
    expect(
      health(status({ backfill, indexing: unresolved }), NOW).parts.at(-1),
    ).toBe("1 failure");
  });
});

describe("share", () => {
  it("shows one decimal and never rounds a share with pages down to zero", () => {
    expect(share(16.7, 312)).toBe("16.7%");
    expect(share(100, 1872)).toBe("100.0%");
    expect(share(0, 3)).toBe("under 0.1%");
    expect(share(0, 0)).toBe("0.0%");
  });
});
