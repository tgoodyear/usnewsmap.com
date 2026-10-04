import type { Activity, Now, Status } from "../api/types";
import { count, relative, span } from "./format";

/** "20:15 UTC" today (in UTC), "Oct 2 at 20:15 UTC" on another day. */
export function utc(iso: string, now: number): string {
  const t = new Date(iso);
  if (Number.isNaN(t.getTime())) return iso;
  const time = `${String(t.getUTCHours()).padStart(2, "0")}:${String(t.getUTCMinutes()).padStart(2, "0")} UTC`;
  const today = new Date(now);
  const sameDay =
    t.getUTCFullYear() === today.getUTCFullYear() &&
    t.getUTCMonth() === today.getUTCMonth() &&
    t.getUTCDate() === today.getUTCDate();
  return sameDay ? time : `${day(t)} at ${time}`;
}

/** "Oct 2 at 20:15 UTC", whatever the day. */
export function utcDate(iso: string): string {
  const t = new Date(iso);
  if (Number.isNaN(t.getTime())) return iso;
  return `${day(t)} at ${String(t.getUTCHours()).padStart(2, "0")}:${String(t.getUTCMinutes()).padStart(2, "0")} UTC`;
}

/** "May 9, 1751" from "1751-05-09". */
export function longDate(ymd: string): string {
  const [y, m, d] = ymd.split("-").map(Number);
  if (!y || !m || !d) return ymd;
  return `${MONTHS[m - 1]} ${d}, ${y}`;
}

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

function day(t: Date): string {
  return `${MONTHS[t.getUTCMonth()]} ${t.getUTCDate()}`;
}

/** "4.1M" from a million up, the full number below. */
export function big(n: number): string {
  if (n < 1_000_000) return count(n);
  return `${(Math.round(n / 100_000) / 10).toFixed(1)}M`;
}

/** A whole percent, or one decimal under 10 ("under 0.1%", "0.4%", "9.9%", "52%"). */
export function pct(done: number, total: number): string {
  if (total <= 0) return "0%";
  const p = (100 * done) / total;
  if (p > 0 && p < 0.1) return "under 0.1%";
  if (p > 0 && p < 10) return `${p.toFixed(1)}%`;
  // Never "100%" before the last one.
  return `${Math.round(p) === 100 && done < total ? 99 : Math.round(p)}%`;
}

/** What each step is called in a sentence ("while …"). */
const DOING: Record<Now, string> = {
  listing: "checking for new batches",
  downloading: "downloading batches",
  titles: "looking up newspaper details",
  indexing: "building the search index",
  merging: "merging the index",
  publishing: "publishing",
  idle: "finishing",
};

export interface NowLine {
  /** The sentence for the current activity. */
  text: string;
  /** Smaller sentences under it: when it started, how the last run ended. */
  notes: string[];
  /** Progress, when the step counts something. */
  progress?: { done: number; total: number; label: string };
}

const SLOW_DOWN = "because loc.gov asked us to slow down";

/** When the site last changed: the last publish the pipeline recorded, else the version's own date. */
export function lastUpdate(s: Status): string {
  return (s.indexing.available && s.indexing.last_published_at) || s.published.published_at;
}

/**
 * The "Right now" line: what the pipeline is doing at this moment, in plain
 * words, with how long it has been going and how far it has got.
 */
export function rightNow(s: Status, now: number): NowLine {
  const a = s.activity;
  if (!a || !a.available) {
    return {
      text: "What the pipeline is doing right now isn't available on this server.",
      notes: [
        `The site searches the update published ${utcDate(lastUpdate(s))}.`,
      ],
    };
  }
  const notes: string[] = [];
  const line = sentence(a, s, now);
  if (a.now !== "idle" && a.since) {
    notes.push(`This step started ${relative(a.since, now)}.`);
  }
  if (a.source === "inferred" && a.reported_at && a.now !== "idle") {
    notes.push(`Progress as of ${utc(a.reported_at, now)}.`);
  }
  const last = lastRun(a, s, now);
  if (last) notes.push(last);
  return { ...line, notes };
}

function sentence(a: Activity, s: Status, now: number): Omit<NowLine, "notes"> {
  const has = a.done !== null && a.total !== null && a.total > 0;
  const done = a.done ?? 0;
  const total = a.total ?? 0;
  const left = a.eta ? Date.parse(a.eta) - now : NaN;
  const eta = left > 60_000 ? `about ${span(left)} left` : "";
  switch (a.now) {
    case "titles": {
      let text = "Looking up newspaper details from the Library of Congress";
      text += has ? `: ${count(done)} of ${count(total)} done (${pct(done, total)}).` : ".";
      const paused = a.paused_until && Date.parse(a.paused_until) > now;
      if (paused) text += ` Paused until ${utc(a.paused_until!, now)} ${SLOW_DOWN}.`;
      else if (eta) text += ` At this pace, ${eta}.`;
      return {
        text,
        progress: has
          ? { done, total, label: `${count(done)} of ${count(total)} newspapers looked up` }
          : undefined,
      };
    }
    case "indexing": {
      if (!has) return { text: "Building the search index." };
      return {
        text: `Building the search index: ${big(done)} of ${big(total)} pages sent (${pct(done, total)})${eta ? `, ${eta}` : ""}.`,
        progress: { done, total, label: `${count(done)} of ${count(total)} pages sent` },
      };
    }
    case "merging":
      return {
        text: "Merging the index (step 3 of 4): every page is in, and its pieces are being combined before it goes live.",
      };
    case "publishing":
      return { text: "Publishing: the new index is going live (step 4 of 4)." };
    case "downloading": {
      let text = "Downloading and processing batches from the Library of Congress";
      text += has ? `: ${count(done)} of ${count(total)} done.` : ".";
      const b = s.backfill;
      if (b.available && b.loc.throttled && b.loc.blocked_until) {
        text += ` Downloads are paused until ${utc(b.loc.blocked_until, now)} ${SLOW_DOWN}.`;
      }
      return {
        text,
        progress: has
          ? { done, total, label: `${count(done)} of ${count(total)} batches processed` }
          : undefined,
      };
    }
    case "listing":
      return { text: "Checking the Library of Congress for new batches." };
    case "idle": {
      let text = `Idle: the last update went live on ${utcDate(lastUpdate(s))}.`;
      text += a.next_run
        ? ` The next scheduled run is ${utcDate(a.next_run)}.`
        : " No run is scheduled.";
      return { text };
    }
  }
}

/** How the last (or, while one runs, the previous) run ended, when that matters. */
function lastRun(a: Activity, s: Status, now: number): string | null {
  const l = a.last;
  if (!l) return null;
  // A run that ended before the update the site shows is history.
  if (Date.parse(l.ended_at) < Date.parse(lastUpdate(s)) && l.outcome !== "published") return null;
  const which = a.now === "idle" ? "The last run" : "The previous run";
  const when = utc(l.ended_at, now);
  const doing = l.step ? DOING[l.step] : null;
  switch (l.outcome) {
    case "failed":
      return `${which} stopped at ${when} because of an error${doing ? ` while ${doing}` : ""}; nothing changed on the site.`;
    case "stopped":
      return `${which} stopped without finishing at ${when}${doing ? `, while ${doing}` : ""}; nothing changed on the site.`;
    case "titles_left":
      return `${which} ran out of time at ${when} while looking up newspaper details, because loc.gov limits how fast we can ask; nothing changed on the site. The next run continues where it stopped.`;
    case "nothing_new":
      return a.now === "idle" ? `${which}, which ended at ${when}, found nothing new to add.` : null;
    case "published":
      return null;
  }
}

export type StepState = "done" | "active" | "paused" | "waiting";

export const STATE_LABEL: Record<StepState, string> = {
  done: "Done",
  active: "In progress",
  paused: "Paused",
  waiting: "Waiting",
};

export interface Step {
  key: "download" | "titles" | "index" | "live";
  title: string;
  /** One line on what the step is. */
  explain: string;
  /** The step's numbers, in words. */
  detail: string;
  state: StepState;
}

/** The four steps a page goes through, with each one's numbers and state. */
export function steps(s: Status, now: number): Step[] {
  const a = s.activity?.available ? s.activity : null;
  const doing = a?.now ?? "idle";
  const b = s.backfill.available ? s.backfill : null;
  const t = s.titles.pipeline.available ? s.titles.pipeline : null;
  const paused = !!(a?.paused_until && Date.parse(a.paused_until) > now);

  const remaining = b ? b.by_status.queued + b.by_status.downloading : null;
  const download: Step = {
    key: "download",
    title: "Downloaded and processed",
    explain:
      "Batches are fetched from the Library of Congress, and each page's text, date and newspaper are pulled out and stored. A batch is how the Library delivers pages: a set of digitized pages from one or more newspapers.",
    detail: b
      ? `${count(b.by_status.curated)} of ${count(b.total)} batches, ${count(b.pages)} pages`
      : "Not known on this server",
    state:
      doing === "downloading" || doing === "listing" || (b?.in_progress ?? 0) > 0
        ? b?.loc.throttled
          ? "paused"
          : "active"
        : remaining === 0 && (b?.total ?? 0) > 0
          ? "done"
          : "waiting",
  };

  const looked = t && t.awaiting_sync !== null ? t.curated_titles - t.awaiting_sync : null;
  let titlesDetail = "Not known on this server";
  if (t && looked !== null) {
    titlesDetail = `${count(looked)} of ${count(t.curated_titles)} newspapers`;
    if (t.batches_waiting_for_titles) {
      titlesDetail += `; ${count(t.batches_waiting_for_titles)} ${t.batches_waiting_for_titles === 1 ? "batch waits" : "batches wait"} for their newspapers' details`;
    }
  }
  const titles: Step = {
    key: "titles",
    title: "Newspaper details looked up",
    explain:
      "Each newspaper's name, place and languages come from loc.gov. A page can't go on the map until its newspaper's details are in.",
    detail: titlesDetail,
    state:
      doing === "titles"
        ? paused
          ? "paused"
          : "active"
        : t?.awaiting_sync === 0
          ? "done"
          : "waiting",
  };

  let indexDetail = "Not known on this server";
  if ((doing === "indexing" || doing === "merging") && a?.done != null && a.total) {
    indexDetail = `${count(a.done)} of ${count(a.total)} pages sent`;
  } else if (doing === "merging") {
    indexDetail = "Every page sent; merging the index";
  } else if (t && t.unpublished_batches !== null) {
    indexDetail =
      t.unpublished_batches === 0
        ? "Every processed batch is in the index"
        : `${count(t.unpublished_batches)} ${t.unpublished_batches === 1 ? "batch isn't" : "batches aren't"} searchable yet; ${count(t.ready_for_release ?? 0)} ${t.ready_for_release === 1 ? "is" : "are"} ready to index`;
  }
  const index: Step = {
    key: "index",
    title: "Indexed",
    explain:
      "The search index is built from pages whose newspaper details are in. Then its many small pieces are merged into a few large ones.",
    detail: indexDetail,
    state:
      doing === "indexing" || doing === "merging"
        ? "active"
        : t?.unpublished_batches === 0
          ? "done"
          : "waiting",
  };

  const p = s.published;
  const live: Step = {
    key: "live",
    title: "Live",
    explain: "The site searches the last published version of the index. Each update adds the pages step 3 indexed.",
    detail: `${count(p.pages)} pages from ${count(p.titles)} newspapers, live since ${utcDate(lastUpdate(s))}`,
    state:
      doing === "publishing"
        ? "active"
        : t?.unpublished_batches === 0 || !t
          ? "done"
          : "waiting",
  };
  return [download, titles, index, live];
}

/** "Searchable now": published pages against every processed page. */
export function headline(s: Status): { text: string; sub: string; share: number | null } {
  const p = s.published;
  // A page downloaded in two batches is searchable once, so it counts once.
  const duplicates = p.duplicate_pages ?? 0;
  const total = s.backfill.available ? s.backfill.pages - duplicates : 0;
  const years = `${p.bounds.from.slice(0, 4)} to ${p.bounds.to.slice(0, 4)}`;
  const sub =
    `From ${count(p.titles)} ${p.titles === 1 ? "newspaper" : "newspapers"}, dated ${years}.` +
    (duplicates > 0
      ? ` ${count(duplicates)} ${duplicates === 1 ? "page ships" : "pages ship"} in two batches and ${duplicates === 1 ? "counts" : "count"} once.`
      : "");
  if (total > 0 && total >= p.pages) {
    return {
      text: `Searchable now: ${count(p.pages)} of ${count(total)} downloaded pages (${pct(p.pages, total)})`,
      sub,
      share: p.pages / total,
    };
  }
  return { text: `Searchable now: ${count(p.pages)} pages`, sub, share: null };
}
