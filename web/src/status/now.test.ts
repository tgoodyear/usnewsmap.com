import { describe, expect, it } from "vitest";
import type { Activity, Status } from "../api/types";
import { big, headline, longDate, pct, rightNow, steps, utc, utcDate } from "./now";

const NOW = Date.parse("2026-10-02T19:30:00Z");
const ago = (min: number) => new Date(NOW - min * 60_000).toISOString();
const later = (min: number) => new Date(NOW + min * 60_000).toISOString();

const idle: Activity = {
  now: "idle",
  source: "none",
  since: null,
  run_started_at: null,
  run: null,
  reported_at: null,
  done: null,
  total: null,
  percent: null,
  eta: null,
  paused_until: null,
  index_version: null,
  merge: null,
  last: null,
  next_run: null,
};

/** The production numbers on 2 October 2026, with `activity` as given. */
function status(activity: Partial<Activity> | null, backfill: Partial<Status["backfill"]> = {}): Status {
  return {
    schema: 1,
    generated_at: new Date(NOW).toISOString(),
    stale: false,
    error: null,
    pipeline: { available: true, read_at: new Date(NOW).toISOString() },
    published: {
      index_version: "pages-v20260929-4",
      published_at: "2026-09-29T13:48:53Z",
      synthetic: false,
      pages: 6_557_925,
      titles: 1217,
      places: 398,
      bounds: { from: "1751-05-09", to: "1963-12-31" },
      indexes: ["pages-base-20260929-1"],
      deltas: 3,
      max_deltas: 8,
      next_release_full: false,
      batches: 882,
    },
    activity: activity === null ? undefined : { available: true, ...idle, ...activity },
    backfill: {
      available: true,
      total: 2997,
      by_status: { queued: 0, downloading: 0, curated: 2997, failed: 0 },
      in_progress: 0,
      stale_leases: 0,
      retrying: 0,
      percent: 100,
      pages: 23_794_152,
      ok_pages: 23_768_831,
      versions: {},
      newer_versions_pending: 0,
      throughput: { hours: [], rate_window_hours: 12, rate_per_hour: 0, remaining: 0, eta: null },
      loc: { next_slot: null, blocked_until: null, throttled: false },
      in_progress_batches: [],
      recent: [],
      failed_batches: [],
      listed_limit: 100,
      ...backfill,
    } as Status["backfill"],
    indexing: {
      available: true,
      current_version: "pages-v20260929-4",
      writer: { held: false, holder: null, until: null },
      release: null,
      last_published_at: "2026-09-29T14:23:36Z",
      failed_runs: 1,
      failed_since_last_publish: 1,
      runs: [],
    },
    titles: {
      catalog: { available: true, titles: 1559, places: 570 },
      published_titles: 1217,
      published_places: 398,
      pipeline: {
        available: true,
        curated_titles: 4681,
        awaiting_sync: 3122,
        batches_waiting_for_titles: 1955,
        unpublished_batches: 2115,
        ready_for_release: 160,
        recurated_awaiting_full: 0,
      },
    },
  };
}

describe("formatting", () => {
  it("writes times in UTC, with the day when it isn't today", () => {
    expect(utc("2026-10-02T20:15:00Z", NOW)).toBe("20:15 UTC");
    expect(utc("2026-10-05T03:17:00Z", NOW)).toBe("Oct 5 at 03:17 UTC");
    expect(utcDate("2026-09-29T14:23:36Z")).toBe("Sep 29 at 14:23 UTC");
    expect(longDate("1751-05-09")).toBe("May 9, 1751");
  });

  it("shortens millions and never rounds up to 100%", () => {
    expect(big(4_123_456)).toBe("4.1M");
    expect(big(999_999)).toBe("999,999");
    expect(pct(6_557_925, 23_794_152)).toBe("28%");
    expect(pct(342, 3464)).toBe("9.9%");
    expect(pct(999, 1000)).toBe("99%");
    expect(pct(1872, 23_794_152)).toBe("under 0.1%");
    expect(pct(1000, 1000)).toBe("100%");
  });
});

describe("rightNow", () => {
  it("titles-sync running, with an estimate", () => {
    const line = rightNow(
      status({ now: "titles", source: "job", since: ago(65), done: 342, total: 3464, eta: later(240) }),
      NOW,
    );
    expect(line.text).toBe(
      "Looking up newspaper details from the Library of Congress: 342 of 3,464 done (9.9%). At this pace, about 4 h left.",
    );
    expect(line.notes[0]).toBe("This step started 1 h 5 min ago.");
    expect(line.progress).toEqual({ done: 342, total: 3464, label: "342 of 3,464 newspapers looked up" });
  });

  it("titles-sync paused by loc.gov", () => {
    const line = rightNow(
      status({
        now: "titles",
        source: "job",
        since: ago(65),
        done: 342,
        total: 3464,
        paused_until: "2026-10-02T20:15:00Z",
        eta: later(240),
      }),
      NOW,
    );
    expect(line.text).toBe(
      "Looking up newspaper details from the Library of Congress: 342 of 3,464 done (9.9%). Paused until 20:15 UTC because loc.gov asked us to slow down.",
    );
  });

  it("titles-sync inferred from its cache says when the progress is from", () => {
    const line = rightNow(
      status({ now: "titles", source: "inferred", done: 342, total: 3122, reported_at: "2026-10-02T19:21:00Z" }),
      NOW,
    );
    expect(line.text).toContain("342 of 3,122 done (11%).");
    expect(line.notes).toContain("Progress as of 19:21 UTC.");
  });

  it("indexing", () => {
    const line = rightNow(
      status({ now: "indexing", source: "job", since: ago(180), done: 4_100_000, total: 7_800_000, eta: later(80) }),
      NOW,
    );
    expect(line.text).toBe(
      "Building the search index: 4.1M of 7.8M pages sent (53%), about 1 h 20 min left.",
    );
    expect(line.notes[0]).toBe("This step started 3 h ago.");
  });

  it("merging", () => {
    const line = rightNow(status({ now: "merging", source: "job", since: ago(20) }), NOW);
    expect(line.text).toBe(
      "Merging the index (step 3 of 4): every page is in, and its pieces are being combined before it goes live.",
    );
    expect(line.progress).toBeUndefined();
  });

  it("merging: where the merge is", () => {
    const merge = { step: "settle", splits: 227, merges_running: 1, merges_queued: 20 };
    expect(rightNow(status({ now: "merging", merge }), NOW).text).toBe(
      "Merging the index (step 3 of 4): every page is in, and its pieces are being combined before it goes live. Index still open: 227 pieces, 1 merge running, 20 waiting.",
    );
    const final = { step: "finalize", splits: 9, merges_running: 2, merges_queued: 0 };
    expect(rightNow(status({ now: "merging", merge: final }), NOW).text).toMatch(
      / Index closed for its final merges: 9 pieces, 2 merges running, 0 waiting\.$/,
    );
    expect(steps(status({ now: "merging", merge }), NOW)[2]!.detail).toBe(
      "Every page sent. Index still open: 227 pieces, 1 merge running, 20 waiting",
    );
  });

  it("publishing", () => {
    expect(rightNow(status({ now: "publishing", source: "job" }), NOW).text).toBe(
      "Publishing: the new index is going live (step 4 of 4).",
    );
  });

  it("downloading, paused by loc.gov", () => {
    const line = rightNow(
      status(
        { now: "downloading", source: "inferred", done: 2990, total: 2997 },
        { loc: { next_slot: null, blocked_until: "2026-10-02T20:31:00Z", throttled: true } },
      ),
      NOW,
    );
    expect(line.text).toBe(
      "Downloading and processing batches from the Library of Congress: 2,990 of 2,997 done. Downloads are paused until 20:31 UTC because loc.gov asked us to slow down.",
    );
  });

  it("idle, with and without a schedule", () => {
    expect(rightNow(status({}), NOW).text).toBe(
      "Idle: the last update went live on Sep 29 at 14:23 UTC. No run is scheduled.",
    );
    expect(rightNow(status({ next_run: "2026-10-05T03:17:00Z" }), NOW).text).toBe(
      "Idle: the last update went live on Sep 29 at 14:23 UTC. The next scheduled run is Oct 5 at 03:17 UTC.",
    );
  });

  it("says the last run failed, in plain words", () => {
    const failed = {
      outcome: "failed" as const,
      ended_at: "2026-10-02T14:09:39Z",
      step: "indexing" as const,
      error: "tcp connect error: Connection refused (os error 111)",
      index_version: "pages-v20261002-1",
    };
    const line = rightNow(status({ last: failed }), NOW);
    expect(line.notes).toContain(
      "The last run stopped at 14:09 UTC because of an error while building the search index; nothing changed on the site.",
    );
    // While a new run is going, it's the previous one.
    const running = rightNow(status({ now: "titles", source: "inferred", done: 1, total: 2, last: failed }), NOW);
    expect(running.notes.some((n) => n.startsWith("The previous run stopped at 14:09 UTC"))).toBe(true);
    // A failure from before the update the site shows is history.
    const old = rightNow(status({ last: { ...failed, ended_at: "2026-09-28T22:49:05Z" } }), NOW);
    expect(old.notes.some((n) => n.includes("stopped"))).toBe(false);
  });

  it("says the last run ran out of time or stopped", () => {
    const last = { ended_at: ago(30), error: null, index_version: null };
    expect(
      rightNow(status({ last: { ...last, outcome: "titles_left", step: "indexing" } }), NOW).notes[0],
    ).toBe(
      "The last run ran out of time at 19:00 UTC while looking up newspaper details, because loc.gov limits how fast we can ask; nothing changed on the site. The next run continues where it stopped.",
    );
    expect(rightNow(status({ last: { ...last, outcome: "stopped", step: "merging" } }), NOW).notes[0]).toBe(
      "The last run stopped without finishing at 19:00 UTC, while merging the index; nothing changed on the site.",
    );
  });

  it("without the activity section", () => {
    expect(rightNow(status(null), NOW).text).toBe(
      "What the pipeline is doing right now isn't available on this server.",
    );
  });
});

describe("steps", () => {
  const states = (s: Status) => steps(s, NOW).map((st) => st.state);

  it("titles-sync running: step 2 is current", () => {
    const s = status({ now: "titles", source: "job", done: 342, total: 3464 });
    expect(states(s)).toEqual(["done", "active", "waiting", "waiting"]);
    const [download, titles, index, live] = steps(s, NOW);
    expect(download!.detail).toBe("2,997 of 2,997 batches, 23,794,152 pages");
    expect(titles!.detail).toBe("1,559 of 4,681 newspapers; 1,955 batches wait for their newspapers' details");
    expect(index!.detail).toBe("2,115 batches aren't searchable yet; 160 are ready to index");
    expect(live!.detail).toBe("6,557,925 pages from 1,217 newspapers, live since Sep 29 at 14:23 UTC");
  });

  it("titles-sync paused", () => {
    expect(states(status({ now: "titles", paused_until: later(45) }))).toEqual([
      "done",
      "paused",
      "waiting",
      "waiting",
    ]);
  });

  it("indexing and merging: step 3 is current", () => {
    const s = status({ now: "indexing", done: 4_100_000, total: 7_800_000 });
    expect(states(s)).toEqual(["done", "waiting", "active", "waiting"]);
    expect(steps(s, NOW)[2]!.detail).toBe("4,100,000 of 7,800,000 pages sent");
    const m = status({ now: "merging" });
    expect(states(m)[2]).toBe("active");
    expect(steps(m, NOW)[2]!.detail).toBe("Every page sent; merging the index");
  });

  it("publishing: step 4 is current", () => {
    expect(states(status({ now: "publishing" }))[3]).toBe("active");
  });

  it("downloading", () => {
    const s = status(
      { now: "downloading" },
      { by_status: { queued: 5, downloading: 2, curated: 2990, failed: 0 }, in_progress: 2 },
    );
    expect(states(s)[0]).toBe("active");
  });

  it("idle and everything published: all done", () => {
    const s = status({});
    s.titles.pipeline = {
      available: true,
      curated_titles: 4681,
      awaiting_sync: 0,
      batches_waiting_for_titles: 0,
      unpublished_batches: 0,
      ready_for_release: 0,
      recurated_awaiting_full: 0,
    };
    expect(states(s)).toEqual(["done", "done", "done", "done"]);
  });
});

describe("headline", () => {
  it("compares searchable pages with every processed page", () => {
    const h = headline(status({}));
    expect(h.text).toBe("Searchable now: 6,557,925 of 23,794,152 downloaded pages (28%)");
    expect(h.sub).toBe("From 1,217 newspapers, dated 1751 to 1963.");
  });

  it("without the pipeline state, only the searchable pages", () => {
    const s = status({});
    s.backfill = { available: false, reason: "none" };
    expect(headline(s).text).toBe("Searchable now: 6,557,925 pages");
  });
});
