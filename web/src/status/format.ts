import type { Status } from "../api/types";

const n = new Intl.NumberFormat("en-US");

export function count(x: number): string {
  return n.format(x);
}

/**
 * A share of all pages, e.g. "16.7%". The API rounds to one decimal, so a
 * share that rounds to 0.0 but has pages reads "under 0.1%", not "0.0%".
 */
export function share(percent: number, pages: number): string {
  if (pages > 0 && percent < 0.1) return "under 0.1%";
  return `${percent.toFixed(1)}%`;
}

/** "45 s", "12 min", "3 h 5 min", "2 d 4 h": a span of time, coarsest two units. */
export function span(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `${s} s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m} min`;
  const h = Math.floor(m / 60);
  if (h < 48) return m % 60 ? `${h} h ${m % 60} min` : `${h} h`;
  const d = Math.floor(h / 24);
  return h % 24 ? `${d} d ${h % 24} h` : `${d} d`;
}

/** "5 min ago" or "in 5 min", relative to `now`. */
export function relative(iso: string, now: number): string {
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return iso;
  // Clocks differ a little between the server and the browser.
  if (Math.abs(now - t) < 10_000) return "just now";
  return t <= now ? `${span(now - t)} ago` : `in ${span(t - now)}`;
}

const dateTime = new Intl.DateTimeFormat("en-US", {
  month: "short",
  day: "numeric",
  hour: "numeric",
  minute: "2-digit",
});

/** A local date and time, e.g. "Sep 29, 2:30 PM". */
export function when(iso: string): string {
  const t = Date.parse(iso);
  return Number.isNaN(t) ? iso : dateTime.format(t);
}

export type Level = "ok" | "attention" | "problem" | "unknown";

export interface Health {
  level: Level;
  /** Short phrases for the header, e.g. "Backfill running". */
  parts: string[];
}

/**
 * The header's one-line summary: what the pipeline is doing, when the last
 * release was, and whether anything failed.
 */
export function health(s: Status, now: number): Health {
  const parts: string[] = [];
  let level: Level = "ok";
  const raise = (l: Level) => {
    const order: Level[] = ["unknown", "ok", "attention", "problem"];
    if (order.indexOf(l) > order.indexOf(level)) level = l;
  };
  const b = s.backfill;
  if (!b.available) {
    parts.push("Pipeline state not available");
    level = "unknown";
  } else if (b.loc.throttled && b.throughput.remaining > 0) {
    parts.push(`Downloads paused by LoC until ${when(b.loc.blocked_until!)}`);
    raise("attention");
  } else if (b.in_progress > 0) {
    parts.push(`Backfill running (${count(b.in_progress)} in progress)`);
  } else if (b.throughput.remaining > 0) {
    parts.push(
      `Backfill idle, ${count(b.throughput.remaining)} batches waiting`,
    );
  } else {
    parts.push("All listed batches curated");
  }
  const i = s.indexing;
  if (i.available && i.release) {
    parts.push(`Release building (${i.release.percent}%)`);
  }
  const last = (i.available && i.last_published_at) || s.published.published_at;
  parts.push(`last release ${relative(last, now)}`);
  if (b.available) {
    // Runs that failed before a later version published are history, not
    // a current problem. Older APIs without the field count every failure.
    const runs = i.available
      ? (i.failed_since_last_publish ?? i.failed_runs)
      : 0;
    const failed = b.by_status.failed + runs;
    if (failed > 0) {
      parts.push(`${count(failed)} ${failed === 1 ? "failure" : "failures"}`);
      raise("attention");
    } else {
      parts.push("no failures");
    }
  }
  if (s.stale) raise("problem");
  return { level, parts };
}
