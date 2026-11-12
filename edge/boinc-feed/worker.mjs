// A dashboard feed for a cloud-boinc project site: GET /feed.json answers the JSON document
// cloud-boinc's work.php shows as panels (schema 1; README.md). It is built from the two public
// endpoints of the OCR dashboard, /api/pipeline.json and /api/view.json, and from nothing else:
// no secrets, no bindings, and nothing is logged but a reason.
//
// Every figure is a value the dashboard shows, or a plain sum its own code defines, under the
// label the dashboard uses (shortened only to fit the feed's limits). A figure the dashboard
// itself would not show (its source is stale, missing or partial) is sent as null, which the
// page shows as "unknown", never as 0.

export const DASHBOARD_URL = "https://ocr.usnewsmap.com/";
export const PIPELINE_URL = "https://ocr.usnewsmap.com/api/pipeline.json";
export const VIEW_URL = "https://ocr.usnewsmap.com/api/view.json";

const ANSWER_SECS = 60; // how long the feed's own answer is reused, here and by the client
const UPSTREAM_TIMEOUT_MS = 2000; // the project site gives the whole exchange 3 seconds
const UPSTREAM_MAX_BYTES = 4 * 1024 * 1024;
const FEED_MAX_BYTES = 64 * 1024;
const LIVE_STALE_SECS = 1800;
const HOURLY_STALE_SECS = 10800; // the dashboard's own budget for its hourly evidence
const MAX_LABEL = 60;
const MAX_UNIT = 24;
const FUTURE_SLACK_SECS = 3600;
const FORTNIGHT_HOURS = 336; // the data-flow catalog's window, in hours: 14 days
const DAY_HOURS = 24;

// ---------- small readers: a wrong shape is "missing", never a guess ----------

const isObject = (x) => x !== null && typeof x === "object" && !Array.isArray(x);
const isNumber = (x) => typeof x === "number" && Number.isFinite(x);
const count = (x) => (isNumber(x) && x >= 0 && x <= 1e18 ? x : null);
const CONTROL = /[\u0000-\u001f\u007f-\u009f]/g;

// Unix seconds from an ISO time or a number of seconds, or null.
function seconds(x) {
  if (isNumber(x)) return Math.floor(x);
  if (typeof x !== "string") return null;
  const t = Date.parse(x);
  return Number.isFinite(t) ? Math.floor(t / 1000) : null;
}

const plain = (s) => String(s).replace(CONTROL, " ").replace(/\s+/g, " ").trim();

// A label within the feed's limit: a trailing ", last 14 days" moves to the unit (by the
// caller); anything still too long is cut at a word and marked with an ellipsis.
export function shorten(label, max = MAX_LABEL) {
  const s = plain(label);
  if (Array.from(s).length <= max) return s;
  const words = Array.from(s.slice(0, max - 1));
  const at = words.lastIndexOf(" ");
  return (at > 0 ? words.slice(0, at) : words).join("").replace(/[\s,;:.]+$/, "") + "…";
}

// ---------- what the feed is made of ----------

function figureOf(label, value, unit, note) {
  const f = { label: shorten(label) };
  f.value = value === undefined ? null : value;
  if (unit) f.unit = shorten(unit, MAX_UNIT);
  if (note) f.note = note;
  return f;
}

// One card. `times` gathers the observation time of every figure that has a value, so the
// card's `updated` is that of its oldest figure.
class Card {
  constructor(title, staleAfter) {
    this.title = title;
    this.staleAfter = staleAfter;
    this.figures = [];
    this.series = [];
    this.times = [];
  }

  figure(label, value, { unit, note, at } = {}) {
    const v = isNumber(value) ? value : null;
    this.figures.push(figureOf(label, v, unit, note));
    if (v !== null && Number.isInteger(at)) this.times.push(at);
  }

  line(title, unit, start, interval, values) {
    if (!Number.isInteger(start) || !values.some((v) => v !== null)) return;
    this.series.push({ title: shorten(title, 80), unit, kind: "bars", start, interval_secs: interval, values });
  }

  done(now) {
    const section = { title: this.title };
    const oldest = Math.min(...this.times);
    // A time the dashboard could not have had yet is no observation time.
    if (this.times.length && oldest >= 1_000_000_000 && oldest <= now + FUTURE_SLACK_SECS) section.updated = oldest;
    section.stale_after_secs = this.staleAfter;
    section.link = { label: "Full dashboard", url: DASHBOARD_URL };
    section.figures = this.figures;
    if (this.series.length) section.series = this.series;
    return section;
  }
}

// ---------- reading the pipeline snapshot like the dashboard does ----------

// The dashboard shows a stage's count only if the stage is observed, has a count, is not
// historical and its freshness entry exists and is not stale (summary_stage in html/ecosystem.rs).
function stageReader(pipeline) {
  const snapshot = isObject(pipeline) && isObject(pipeline.snapshot) ? pipeline.snapshot : null;
  const stages = snapshot && Array.isArray(snapshot.stages) ? snapshot.stages : [];
  const freshness = isObject(pipeline) && Array.isArray(pipeline.stage_freshness) ? pipeline.stage_freshness : [];
  const overall = snapshot ? seconds(snapshot.observed_at) : null;
  return function stage(id) {
    const s = stages.find((x) => isObject(x) && x.id === id);
    const f = freshness.find((x) => isObject(x) && x.id === id);
    const ok = s && f && s.state === "observed" && count(s.count) !== null && s.historical !== true && f.stale === false;
    return {
      label: s && typeof s.label === "string" && s.label.trim() ? s.label : null,
      value: ok ? s.count : null,
      at: ok ? (seconds(s.observed_at) ?? overall) : null,
      detail: s ? s.detail : undefined,
    };
  };
}

// A 14-day total counts only if the catalog had every hour of the window: "Last 336 hours, 336
// reported." (fortnight in html/overview.rs). With fewer hours it is an undercount.
function fortnight(detail) {
  const m = typeof detail === "string" ? /^Last (\d+) hours, (\d+) reported\./.exec(detail) : null;
  return m !== null && Number(m[1]) === FORTNIGHT_HOURS && Number(m[2]) === FORTNIGHT_HOURS;
}

const LAST_14 = /,\s*last 14 days$/;

// A card figure from a pipeline stage. `unit` is the noun the label counts; a stage whose label
// ends ", last 14 days" shows that under the figure instead, and shows nothing unless the
// catalog had every hour of the window. A note, where there is one, is a sentence from the
// dashboard's own text that keeps the count from being misread; most of its details name
// internals a volunteer has no use for, so they are not passed on.
function stageFigure(card, stage, id, fallback, unit, { note } = {}) {
  const s = stage(id);
  const label = s.label ?? fallback;
  const windowed = LAST_14.test(label);
  const known = !windowed || fortnight(s.detail);
  card.figure(label.replace(LAST_14, ""), known ? s.value : null, {
    unit: windowed ? `${unit}, last 14 days` : unit,
    note,
    at: s.at ?? undefined,
  });
}

// ---------- the sections ----------

const APPS = ["usnm_ocr", "usnm_qwen", "usnm_quality", "usnm_gold"];

function jobsInFlight(stage, now) {
  const card = new Card("Jobs in flight", LIVE_STALE_SECS);
  stageFigure(card, stage, "boinc_unsent", "BOINC unsent result replicas", "replicas", {
    note: "Result replicas, not pages or GPU eligibility.",
  });
  stageFigure(card, stage, "boinc_in_progress", "BOINC assigned result replicas", "replicas", {
    note: "Assigned does not mean actively computing.",
  });
  for (const name of APPS) {
    stageFigure(card, stage, `boinc_${name}_unsent`, `${name}: unsent replicas`, "replicas");
    stageFigure(card, stage, `boinc_${name}_in_progress`, `${name}: assigned replicas`, "replicas");
  }
  return card.done(now);
}

// What a page's journey is, in the order it takes: waiting, in flight, then done. These are
// separate populations (the dashboard says so), so each keeps its own label and note.
function pipelineCard(stage, now) {
  const card = new Card("Pipeline", HOURLY_STALE_SECS);
  stageFigure(card, stage, "queued", "Pages in the ocr-bulk queue", "pages");
  stageFigure(card, stage, "allocated", "OCR pages at BOINC or awaiting collection", "pages");
  stageFigure(card, stage, "validated", "BOINC OCR workunits with a canonical result, last 14 days", "workunits", {
    note: "A bundled workunit holds an unknown number of pages, so this counts workunits.",
  });
  stageFigure(card, stage, "collected", "OCR pages collected through hand-off checks, last 14 days", "pages");
  stageFigure(card, stage, "imported", "OCR pages in committed imports, last 14 days", "pages");
  stageFigure(card, stage, "rejected", "OCR parts rejected or dead-lettered, last 14 days", "parts", {
    note: "Attempts, not pages lost.",
  });
  return card.done(now);
}

// The fetcher's records are partial while the dashboard warms up or cannot read them, and it
// says so; the feed then gives no fetch count at all.
function fetchCounts(view) {
  const f = isObject(view) && isObject(view.fetch) ? view.fetch : null;
  if (!f || view.warming != null || f.index_error != null) return null;
  return f;
}

// Pages queued but not fetched, by what they were picked for (left_by_kind in html/overview.rs).
function leftByKind(queue) {
  if (!Array.isArray(queue)) return null;
  let ocr = 0;
  let quality = 0;
  for (const m of queue) {
    if (!isObject(m)) return null;
    if (!["queued", "fetching", "cooling"].includes(m.state)) continue;
    const pages = count(m.pages);
    const fetched = count(m.fetched);
    if (pages === null || fetched === null) return null;
    const left = Math.max(pages - fetched, 0);
    if (m.kind === "quality") quality += left;
    else ocr += left;
  }
  return { ocr, quality };
}

function whatsNext(view, now) {
  const card = new Card("What's next", LIVE_STALE_SECS);
  const f = fetchCounts(view);
  const at = seconds(view && view.generated_at) ?? undefined;
  const queueOk = f && f.queue_configured === true && f.queue_error == null && isObject(f.queue_summary);
  const left = queueOk ? count(f.queue_summary.pages_left) : null;
  const byKind = queueOk ? leftByKind(f.queue) : null;
  const lastDay = f ? count(isObject(f.last_day) ? f.last_day.pages : null) : null;
  card.figure("Pages still to fetch", left, {
    unit: "pages",
    note: "In manifests the queue will still run (queued, fetching or cooling off).",
    at,
  });
  card.figure("Pages still to fetch for our OCR", byKind && left !== null ? byKind.ocr : null, { unit: "pages", at });
  card.figure("Pages still to fetch for the quality checks", byKind && left !== null ? byKind.quality : null, {
    unit: "pages",
    at,
  });
  // "At the last day's pace, that takes about N days": the pages left over the pages fetched
  // in the last 24 hours, rounded up. Without a last day's pace there is no such sentence.
  card.figure("Days to fetch the rest at the last day's pace", left !== null && lastDay ? Math.ceil(left / lastDay) : null, {
    unit: "days",
    note: "Pages still to fetch divided by pages fetched in the last 24 hours, rounded up.",
    at,
  });
  return card.done(now);
}

function doneCard(stage, view, now) {
  const card = new Card("What's been done", HOURLY_STALE_SECS);
  const f = fetchCounts(view);
  const at = seconds(view && view.generated_at) ?? undefined;
  card.figure("Page scans fetched so far", f && isObject(f.total) ? count(f.total.pages) : null, { unit: "pages", at });
  stageFigure(card, stage, "indexed", "Pages searchable with our OCR", "pages");
  stageFigure(card, stage, "quality_loaded", "Quality and Qwen pages loaded, last 14 days", "pages");
  return card.done(now);
}

// What volunteers' computers do for each app, in the dashboard's words (APP_WORK in
// html/overview.rs). Two of them name the same work and are added together there, as here. The
// quality-check phrase is shortened to fit a label.
const APP_WORK = {
  usnm_gold: "jobs testing a volunteer's OCR against known text",
  usnm_gold_native: "jobs testing a volunteer's OCR against known text",
  usnm_ndlocr: "jobs reading Japanese pages",
  usnm_ocr: "jobs reading newspaper pages with OCR",
  usnm_quality: "jobs scoring page quality by comparing a page's OCR texts",
  usnm_qwen: "jobs checking quality with an AI vision model that reads snippets of pages",
};
const OTHER_JOBS = "other jobs";
const capital = (s) => s[0].toUpperCase() + s.slice(1);

function volunteers(view, now) {
  const card = new Card("Volunteers", LIVE_STALE_SECS);
  const b = isObject(view) && isObject(view.boinc_week) ? view.boinc_week : null;
  const week = b && b.stale === false && isObject(b.week) && Array.isArray(b.week.buckets) ? b.week : null;
  const buckets = week ? week.buckets.filter((x) => isObject(x) && isObject(x.completed)) : [];
  const at = week ? (seconds(b.generated_at) ?? undefined) : undefined;
  if (buckets.length === 0) {
    card.figure("Volunteers active in the last 3 hours", null);
    card.figure("Jobs finished in the last 24 hours", null);
    return card.done(now);
  }
  // The last eight full 3-hour buckets are the last 24 hours up to the last boundary.
  const full = buckets.filter((x) => x.partial === false);
  const day = full.slice(Math.max(full.length - 8, 0));
  const last = full.length ? full[full.length - 1] : buckets[buckets.length - 1];
  const sum = (o) => Object.values(o).reduce((s, n) => s + (count(n) ?? NaN), 0);
  const finished = day.map((x) => sum(x.completed));
  card.figure("Volunteers active in the last 3 hours", count(last.volunteers), { unit: "volunteers", at });
  card.figure("Jobs finished in the last 24 hours", finished.every(isNumber) ? finished.reduce((s, n) => s + n, 0) : null, {
    unit: "jobs",
    at,
  });
  const mix = new Map();
  for (const x of day) {
    for (const [app, n] of Object.entries(x.completed)) {
      const what = APP_WORK[app] ?? OTHER_JOBS;
      mix.set(what, (mix.get(what) ?? 0) + (count(n) ?? 0));
    }
  }
  const rows = [...mix].filter(([, n]) => n > 0).sort((a, b2) => b2[1] - a[1] || (a[0] < b2[0] ? -1 : 1));
  for (const [what, n] of rows) {
    const label = what === APP_WORK.usnm_qwen ? "jobs checking quality with an AI vision model" : what;
    card.figure(capital(label), n, { unit: "last 24 hours", at });
  }
  return card.done(now);
}

function stepOf(view, id) {
  const steps = isObject(view) && isObject(view.throughput) && Array.isArray(view.throughput.steps) ? view.throughput.steps : [];
  const s = steps.find((x) => isObject(x) && x.id === id);
  return s ?? null;
}

// A step's day totals, oldest first from throughput.days_start; a day not counted for all 24
// hours is unknown, as the dashboard leaves it out of its average ("a gap isn't a slow day").
function dayValues(step) {
  if (!step || !Array.isArray(step.days)) return [];
  return step.days.map((d) =>
    isObject(d) && d.hours_known === DAY_HOURS && count(d.value) !== null ? d.value : null,
  );
}

function throughput(view, now) {
  const card = new Card("Throughput", HOURLY_STALE_SECS);
  // `throughput.error` is the warning from the dashboard's start, when it could not read its saved
  // history. It stays set after new data comes in, and the dashboard still shows its steps beside
  // it, so it does not hide them here either: the per-day and per-hour counts below say what was
  // counted in full.
  const t = isObject(view) && isObject(view.throughput) ? view.throughput : null;
  // The oldest observation time any source has. A source with no time is left out of that, not
  // allowed to erase the times the others have.
  const sources = t && Array.isArray(t.sources) ? t.sources.map((s) => (isObject(s) ? seconds(s.observed_at) : null)) : [];
  const known = sources.filter((x) => x !== null);
  const at = known.length ? Math.min(...known) : undefined;
  const fetchAt = seconds(view && view.generated_at) ?? undefined;
  const f = fetchCounts(view);

  // The dashboard's "Last 24 hours" table: the scan fetch's own 24 hours, the others only if
  // all 24 hours were counted.
  const lastDay = (id) => {
    const s = t && stepOf(view, id);
    return s && s.day_hours_known === DAY_HOURS ? count(s.last_day) : null;
  };
  card.figure("Scans fetched", f && isObject(f.last_day) ? count(f.last_day.pages) : null, {
    unit: "last 24 hours",
    at: fetchAt,
  });
  card.figure("OCR pages imported", lastDay("ocr_import"), { unit: "last 24 hours", at });
  card.figure("Pages scored by the quality checks", lastDay("quality_load"), { unit: "last 24 hours", at });

  const imported = dayValues(t && stepOf(view, "ocr_import"));
  const whole = imported.filter(isNumber);
  const average = whole.length ? Math.round(whole.reduce((s, n) => s + n, 0) / whole.length) : null;
  card.figure("OCR pages imported a day", average, {
    unit: whole.length === imported.length ? "7-day average" : "average of whole days",
    note:
      whole.length === imported.length || whole.length === 0
        ? undefined
        : `Average of the ${whole.length} fully counted days of the last 7.`,
    at,
  });

  // Replicas back in the last full hour, across the three apps, and the valid ones among them
  // (ThroughputView in throughput.rs). The JSON carries them; the HTML page does not show them.
  card.figure("Results back in the last full hour", t ? count(t.results_last_hour) : null, {
    unit: "replicas",
    note: "Across the OCR, quality and Qwen apps, whatever the outcome.",
    at,
  });
  card.figure("Valid results in the last full hour", t ? count(t.valid_last_hour) : null, {
    unit: "replicas",
    note: "Of those, the valid ones.",
    at,
  });

  // Seven bars, a day each, from days_start: the 24 hours up to each day's end. The scan fetch's
  // bars count the fetch index like its scalar above, so they wait for the same records.
  const start = t ? seconds(t.days_start) : null;
  const fetched = dayValues(f && t && stepOf(view, "scan_fetch"));
  card.line("Pages fetched a day, last 7 days", "pages", start, 86400, fetched);
  card.line("OCR pages imported a day, last 7 days", "pages", start, 86400, imported);
  return card.done(now);
}

// ---------- the document ----------

export function buildFeed(pipeline, view, now) {
  const stage = stageReader(pipeline);
  const sections = [
    jobsInFlight(stage, now),
    pipelineCard(stage, now),
    whatsNext(view, now),
    doneCard(stage, view, now),
    volunteers(view, now),
    throughput(view, now),
  ];
  return { schema: 1, sections };
}

// ---------- fetching, caching, answering ----------

const JSON_HEADERS = {
  "content-type": "application/json; charset=utf-8",
  "x-content-type-options": "nosniff",
};

function reply(body, status, maxAge, extra = {}) {
  const cache = maxAge > 0 ? `public, max-age=${maxAge}` : "no-store";
  return new Response(body, { status, headers: { ...JSON_HEADERS, "cache-control": cache, ...extra } });
}

// The upstream request: a short limit for the whole exchange, no redirects, and the edge cache
// told to keep a good answer for a minute (and an error for none), so a location asks the
// dashboard at most once a minute per endpoint however often the feed is asked for.
async function upstream(fetchImpl, url, signal, max) {
  const response = await fetchImpl(url, {
    method: "GET",
    headers: { accept: "application/json" },
    redirect: "manual",
    signal,
    cf: { cacheEverything: true, cacheTtlByStatus: { "200-299": ANSWER_SECS, "300-599": 0 } },
  });
  if (response.status !== 200) throw new Error(`status ${response.status}`);
  const length = Number(response.headers.get("content-length"));
  if (length > UPSTREAM_MAX_BYTES) throw new Error("too large");
  return JSON.parse(await readLimited(response, max));
}

// The body as text, read in pieces and counted in bytes: past the limit the stream is cancelled
// and nothing more is read, so a body with no Content-Length cannot fill the Worker's memory.
async function readLimited(response, max) {
  if (!response.body) throw new Error("no body");
  const reader = response.body.getReader();
  const chunks = [];
  let total = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    total += value.byteLength;
    if (total > max) {
      await reader.cancel().catch(() => {});
      throw new Error("too large");
    }
    chunks.push(value);
  }
  const bytes = new Uint8Array(total);
  let at = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, at);
    at += chunk.byteLength;
  }
  return new TextDecoder().decode(bytes);
}

// The pieces a handler needs, so tests can supply them. `cache` is the Cache API's default
// cache (or anything with match and put); `now` is in milliseconds.
export function createHandler({
  fetchImpl,
  cache,
  now = () => Date.now(),
  timeoutMs = UPSTREAM_TIMEOUT_MS,
  maxBytes = UPSTREAM_MAX_BYTES,
}) {
  // The last document built in this isolate, and the build in progress, so a burst of requests
  // makes one round of upstream requests.
  let memo = null;
  let building = null;

  async function fetchOne(url) {
    try {
      return await upstream(fetchImpl, url, AbortSignal.timeout(timeoutMs), maxBytes);
    } catch (e) {
      // The reason only: never the address or the body.
      console.error("upstream unavailable:", e && e.name ? e.name : "error");
      return null;
    }
  }

  async function build() {
    const [pipeline, view] = await Promise.all([fetchOne(PIPELINE_URL), fetchOne(VIEW_URL)]);
    if (pipeline === null && view === null) return null;
    const builtAt = Math.floor(now() / 1000);
    let body;
    try {
      body = JSON.stringify(buildFeed(pipeline, view, builtAt));
    } catch (e) {
      console.error("feed build failed:", e && e.name ? e.name : "error");
      return null;
    }
    if (new TextEncoder().encode(body).length > FEED_MAX_BYTES) {
      console.error("feed over the size limit");
      return null;
    }
    return { at: now(), body, builtAt };
  }

  return async function handle(request) {
    const url = new URL(request.url);
    if (url.pathname !== "/feed.json") return reply('{"error":"not found"}', 404, 0);
    if (request.method !== "GET") return reply('{"error":"method not allowed"}', 405, 0, { allow: "GET" });

    if (fresh()) return answer(memo);
    const key = new Request(`${url.origin}/feed.json`);
    const cached = await lookup(key);
    if (cached) return cached;
    // The lookup took time: another request may have built the feed meanwhile.
    if (fresh()) return answer(memo);

    building ??= build().finally(() => {
      building = null;
    });
    const built = await building;
    if (built === null) return reply('{"error":"dashboard unavailable"}', 502, 10);
    memo = built;
    const response = answer(built);
    if (cache) {
      try {
        await cache.put(key, response.clone());
      } catch (e) {
        console.error("cache put failed:", e && e.name ? e.name : "error");
      }
    }
    return response;
  };

  function fresh() {
    return memo !== null && now() - memo.at < ANSWER_SECS * 1000;
  }

  // A cache that cannot be read is a cache with nothing in it.
  async function lookup(key) {
    if (!cache) return undefined;
    try {
      return await cache.match(key);
    } catch (e) {
      console.error("cache match failed:", e && e.name ? e.name : "error");
      return undefined;
    }
  }

  function answer(built) {
    return reply(built.body, 200, ANSWER_SECS, { "x-feed-built-at": String(built.builtAt) });
  }
}

let handler;

export default {
  async fetch(request) {
    handler ??= createHandler({ fetchImpl: fetch, cache: caches.default });
    return handler(request);
  },
};
