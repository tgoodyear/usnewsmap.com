import type { Activity, Now, Status } from "../api/types";
import { count, relative, span } from "./format";

// Times are in the visitor's own time zone, with the zone named.
const clock = new Intl.DateTimeFormat("en-US", {
  hour: "numeric",
  minute: "2-digit",
  timeZoneName: "short",
});
const calendarDay = new Intl.DateTimeFormat("en-US", {
  year: "numeric",
  month: "numeric",
  day: "numeric",
});
const monthDay = new Intl.DateTimeFormat("en-US", {
  month: "short",
  day: "numeric",
});

/** "4:15 PM EDT" today (the visitor's today), "Oct 2 at 4:15 PM EDT" on another day. */
export function localTime(iso: string, now: number): string {
  const t = new Date(iso);
  if (Number.isNaN(t.getTime())) return iso;
  const sameDay = calendarDay.format(t) === calendarDay.format(new Date(now));
  return sameDay ? hm(t) : localDate(iso);
}

/** "Oct 2 at 4:15 PM EDT", whatever the day. */
export function localDate(iso: string): string {
  const t = new Date(iso);
  if (Number.isNaN(t.getTime())) return iso;
  return `${monthDay.format(t)} at ${hm(t)}`;
}

/** ICU puts a narrow no-break space before AM/PM; plain text wants a space. */
function hm(t: Date): string {
  return clock.format(t).replace(/\u202f/g, " ");
}

/** "May 9, 1751" from "1751-05-09". */
export function longDate(ymd: string): string {
  const [y, m, d] = ymd.split("-").map(Number);
  if (!y || !m || !d) return ymd;
  return `${MONTHS[m - 1]} ${d}, ${y}`;
}

const MONTHS = [
  "Jan",
  "Feb",
  "Mar",
  "Apr",
  "May",
  "Jun",
  "Jul",
  "Aug",
  "Sep",
  "Oct",
  "Nov",
  "Dec",
];

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

/**
 * When the version the site searches went live: its own run's publish time.
 * Without that run, the pipeline's last publish, but only when nothing says
 * it belongs to another version: `last_published_at` is the latest publish
 * of any run, and a new run is marked published just before `ops/current`
 * names it. Last, the version's own `published_at` (versions published
 * before 5 October 2026 carry their run's start there).
 */
export function lastUpdate(s: Status): string {
  const i = s.indexing;
  const served = s.published.index_version;
  if (i.available) {
    const run = i.runs.find(
      (r) => r.index_version === served && r.published_at,
    );
    if (run?.published_at) return run.published_at;
    const last = i.last_published_at;
    const another = i.runs.some(
      (r) => r.published_at === last && r.index_version !== served,
    );
    if (last && i.current_version === served && !another) return last;
  }
  return s.published.published_at;
}

/**
 * A newer version the pipeline has published that the API doesn't serve yet
 * (it checks every few minutes, then warms the version up): when it was
 * published, else null. A version that differs but isn't newer (a pipeline
 * reading older than the API's switch) is not one.
 */
export function pendingUpdate(s: Status): string | null {
  const i = s.indexing;
  if (
    !i.available ||
    !i.current_version ||
    i.current_version === s.published.index_version
  ) {
    return null;
  }
  const at =
    i.runs.find((r) => r.index_version === i.current_version && r.published_at)
      ?.published_at ?? i.last_published_at;
  if (!at || !(Date.parse(at) > Date.parse(lastUpdate(s)))) return null;
  return at;
}

function pendingNote(s: Status): string | null {
  const at = pendingUpdate(s);
  if (at === null) return null;
  return `A new update was published on ${localDate(at)}, and the site switches to it in a few minutes. Until then, the numbers on this page are from the update it still searches.`;
}

/**
 * The "Right now" line: what the pipeline is doing at this moment, in plain
 * words, with how long it has been going and how far it has got.
 */
export function rightNow(s: Status, now: number): NowLine {
  const a = s.activity;
  const pending = pendingNote(s);
  if (!a || !a.available) {
    return {
      text: "What the pipeline is doing right now isn't available on this server.",
      notes: [
        ...(pending ? [pending] : []),
        `The site searches the update published ${localDate(lastUpdate(s))}.`,
      ],
    };
  }
  const notes: string[] = pending ? [pending] : [];
  const line = sentence(a, s, now);
  if (a.now !== "idle" && a.since) {
    notes.push(`This step started ${relative(a.since, now)}.`);
  }
  if (a.source === "inferred" && a.reported_at && a.now !== "idle") {
    notes.push(`Progress as of ${localTime(a.reported_at, now)}.`);
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
      text += has
        ? `: ${count(done)} of ${count(total)} done (${pct(done, total)}).`
        : ".";
      const paused = a.paused_until && Date.parse(a.paused_until) > now;
      if (paused)
        text += ` Paused until ${localTime(a.paused_until!, now)} ${SLOW_DOWN}.`;
      else if (eta) text += ` At this pace, ${eta}.`;
      return {
        text,
        progress: has
          ? {
              done,
              total,
              label: `${count(done)} of ${count(total)} newspapers looked up`,
            }
          : undefined,
      };
    }
    case "indexing": {
      if (!has) return { text: "Building the search index." };
      return {
        text: `Building the search index: ${big(done)} of ${big(total)} pages sent (${pct(done, total)})${eta ? `, ${eta}` : ""}.`,
        progress: {
          done,
          total,
          label: `${count(done)} of ${count(total)} pages sent`,
        },
      };
    }
    case "merging": {
      let text =
        "Merging the index (step 3 of 4): every page is in, and its pieces are being combined before it goes live.";
      const m = mergeDetail(a);
      if (m) text += ` ${m}.`;
      return { text };
    }
    case "publishing":
      return { text: "Publishing: the new index is going live (step 4 of 4)." };
    case "downloading": {
      let text =
        "Downloading and processing batches from the Library of Congress";
      text += has ? `: ${count(done)} of ${count(total)} done.` : ".";
      const b = s.backfill;
      if (b.available && b.loc.throttled && b.loc.blocked_until) {
        text += ` Downloads are paused until ${localTime(b.loc.blocked_until, now)} ${SLOW_DOWN}.`;
      }
      return {
        text,
        progress: has
          ? {
              done,
              total,
              label: `${count(done)} of ${count(total)} batches processed`,
            }
          : undefined,
      };
    }
    case "listing":
      return { text: "Checking the Library of Congress for new batches." };
    case "idle": {
      let text = `Idle: the last update went live on ${localDate(lastUpdate(s))}.`;
      text += a.next_run
        ? ` The next scheduled run is ${localDate(a.next_run)}.`
        : " No run is scheduled.";
      return { text };
    }
  }
}

/**
 * Where the merge is: "settle" while the open index merges in the background,
 * "finalize" once it is closed and its last pieces are merged.
 */
function mergeDetail(a: Activity | null | undefined): string | null {
  const m = a?.merge;
  if (!m) return null;
  const pieces = `${count(m.splits)} ${m.splits === 1 ? "piece" : "pieces"}`;
  const work = `${count(m.merges_running)} ${m.merges_running === 1 ? "merge" : "merges"} running, ${count(m.merges_queued)} waiting`;
  return m.step === "finalize"
    ? `Index closed for its final merges: ${pieces}, ${work}`
    : `Index still open: ${pieces}, ${work}`;
}

/** How the last (or, while one runs, the previous) run ended, when that matters. */
function lastRun(a: Activity, s: Status, now: number): string | null {
  const l = a.last;
  if (!l) return null;
  // A run that ended before the update the site shows is history.
  if (
    Date.parse(l.ended_at) < Date.parse(lastUpdate(s)) &&
    l.outcome !== "published"
  )
    return null;
  const which = a.now === "idle" ? "The last run" : "The previous run";
  const when = localTime(l.ended_at, now);
  const doing = l.step ? DOING[l.step] : null;
  switch (l.outcome) {
    case "failed":
      return `${which} stopped at ${when} because of an error${doing ? ` while ${doing}` : ""}; nothing changed on the site.`;
    case "stopped":
      return `${which} stopped without finishing at ${when}${doing ? `, while ${doing}` : ""}; nothing changed on the site.`;
    case "titles_left":
      return `${which} ran out of time at ${when} while looking up newspaper details, because loc.gov limits how fast we can ask; nothing changed on the site. The next run continues where it stopped.`;
    case "nothing_new":
      return a.now === "idle"
        ? `${which}, which ended at ${when}, found nothing new to add.`
        : null;
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
      doing === "downloading" ||
      doing === "listing" ||
      (b?.in_progress ?? 0) > 0
        ? b?.loc.throttled
          ? "paused"
          : "active"
        : remaining === 0 && (b?.total ?? 0) > 0
          ? "done"
          : "waiting",
  };

  const looked =
    t && t.awaiting_sync !== null ? t.curated_titles - t.awaiting_sync : null;
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
  if (
    (doing === "indexing" || doing === "merging") &&
    a?.done != null &&
    a.total
  ) {
    indexDetail = `${count(a.done)} of ${count(a.total)} pages sent`;
  } else if (doing === "merging") {
    const m = mergeDetail(a);
    indexDetail = m
      ? `Every page sent. ${m}`
      : "Every page sent; merging the index";
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
    explain:
      "The site searches the last published version of the index. Each update adds the pages step 3 indexed.",
    detail: `${count(p.pages)} pages from ${count(p.titles)} newspapers, live since ${localDate(lastUpdate(s))}`,
    state:
      doing === "publishing"
        ? "active"
        : t?.unpublished_batches === 0 || !t
          ? "done"
          : "waiting",
  };
  return [download, titles, index, live];
}

/**
 * "Searchable now": published pages against every processed page. The share
 * compares downloaded copies on both sides: the backfill counts every copy of
 * a page that ships in two batches, and the published version left out
 * `duplicate_pages` such copies, so those are added back to its pages.
 */
export function headline(s: Status): {
  text: string;
  sub: string;
  share: number | null;
} {
  const p = s.published;
  const duplicates = p.duplicate_pages ?? 0;
  const copies = p.pages + duplicates;
  const total = s.backfill.available ? s.backfill.pages : 0;
  const years = `${p.bounds.from.slice(0, 4)} to ${p.bounds.to.slice(0, 4)}`;
  const sub =
    `From ${count(p.titles)} ${p.titles === 1 ? "newspaper" : "newspapers"}, dated ${years}.` +
    (duplicates > 0
      ? ` ${count(duplicates)} of the downloaded pages ${duplicates === 1 ? "is a copy" : "are copies"} of pages in another batch, searchable once.`
      : "");
  if (total > 0 && total >= copies) {
    return {
      text:
        duplicates > 0
          ? `Searchable now: ${count(p.pages)} pages, from ${count(copies)} of ${count(total)} downloaded pages (${pct(copies, total)})`
          : `Searchable now: ${count(p.pages)} of ${count(total)} downloaded pages (${pct(p.pages, total)})`,
      sub,
      share: copies / total,
    };
  }
  return { text: `Searchable now: ${count(p.pages)} pages`, sub, share: null };
}

export interface OcrLine {
  /** What the OCR job is doing, in a sentence. */
  text: string;
  /** Pages read of the target pages, while there are any. */
  progress?: { done: number; total: number; label: string };
  /** How many are searchable, or when they will be. */
  searchable: string;
}

/**
 * "OCR experiments": our own text recognition of the Japanese pages LoC has
 * no text for (`ocr_ja`, #139). Not a pipeline step; `null` when this server
 * has nothing to say about it (no report and no Japanese index).
 */
export function ocrExperiment(s: Status, now: number): OcrLine | null {
  const o = s.ocr_ja?.available ? s.ocr_ja : null;
  const ja = s.published.ja;
  if (!o && !ja) return null;
  const searchable = ja
    ? `${count(ja.pages)} ${ja.pages === 1 ? "page is" : "pages are"} searchable now.`
    : "They become searchable with the next update.";
  if (!o)
    return {
      text: "Japanese pages we read ourselves are in the search.",
      searchable,
    };
  const done = o.done.pages;
  const total = o.targets.pages;
  // No bar for an empty target list: a zero-length range says nothing.
  const progress =
    total > 0
      ? {
          done,
          total,
          label: `${count(done)} of ${count(total)} Japanese pages read`,
        }
      : undefined;
  if (total === 0)
    return { text: "There are no Japanese pages to read.", searchable };
  if (done >= total) {
    return {
      text: `All ${count(total)} Japanese pages read.`,
      progress,
      searchable,
    };
  }
  if (o.running) {
    const left = o.eta ? Date.parse(o.eta) - now : NaN;
    return {
      text: `Reading Japanese pages: ${count(done)} of ${count(total)} done (${pct(done, total)})${left > 60_000 ? `, about ${span(left)} left` : ""}.`,
      progress,
      searchable,
    };
  }
  return {
    text: `Stopped at ${count(done)} of ${count(total)} Japanese pages (${pct(done, total)}). The job last reported ${relative(o.updated_at, now)}.`,
    progress,
    searchable,
  };
}

export interface AuditLine {
  text: string;
  progress?: { done: number; total: number; label: string };
  /** Once finished: how often a page's language isn't its newspaper's first. */
  agreement?: string;
}

const share = (x: number) => (x > 0 && x < 0.001 ? "under 0.1%" : `${(x * 100).toFixed(1)}%`);

/** A rate or share in the audit's table: "–" without a word list, never "0.0%" for one observed. */
export function auditRate(x: number | null): string {
  return x === null ? "–" : share(x);
}

/**
 * The OCR quality audit (`ocr_quality`): a sample of pages, each page's
 * language and how damaged its OCR text is. `null` when it has never run.
 */
export function ocrAudit(s: Status, now: number): AuditLine | null {
  const q = s.ocr_quality?.available ? s.ocr_quality : null;
  if (!q) return null;
  const { done, total } = q.batches;
  const progress = total > 0 ? { done, total, label: `${count(done)} of ${count(total)} batches checked` } : undefined;
  if (q.finished_at && q.summary) {
    const a = q.summary.agreement;
    return {
      text: `Checked ${count(q.pages_sampled)} pages, a ${q.sample_pct}% sample of every batch, on ${localDate(q.finished_at)}.`,
      agreement: `On ${share(a.differs_share)} of pages with text, the language we detected is not the first language in the newspaper's catalog record (${share(a.multilingual_differs_share)} for newspapers whose record lists more than one). That includes ${share(a.mixed_share)} that mix two languages and ${share(a.und_share)} we could not place: too short, too garbled, or in a language we have no word list for.`,
    };
  }
  if (q.running) {
    return {
      text: `Checking a ${q.sample_pct}% sample of pages: ${count(done)} of ${count(total)} batches (${pct(done, total)}), ${count(q.pages_sampled)} pages so far.`,
      progress,
    };
  }
  return {
    text: `The audit stopped at ${count(done)} of ${count(total)} batches. It last reported ${relative(q.updated_at, now)}.`,
    progress,
  };
}
