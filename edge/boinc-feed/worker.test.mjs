// node --test edge/boinc-feed/worker.test.mjs
// Offline: the mapping runs on trimmed copies of the two dashboard documents (fixtures/), and
// the Worker's fetching and caching run against stand-ins for fetch and the Cache API.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import worker, { buildFeed, createHandler, shorten, PIPELINE_URL, VIEW_URL } from "./worker.mjs";

const fixture = (name) => JSON.parse(readFileSync(new URL(`./fixtures/${name}.json`, import.meta.url), "utf8"));
const pipeline = () => fixture("pipeline");
const view = () => fixture("view");
const NOW = Math.floor(Date.parse(view().generated_at) / 1000) + 10;

// ---------- the project site's rules for a feed (cloud/docs/dashboard-feed.md, cb-core's feed.rs) ----------

const MAX = { sections: 8, figures: 12, series: 3, points: 200, title: 80, label: 60, value: 40, unit: 24, note: 160, url: 300 };
const MIN_TIME = 1_000_000_000;
const DAY = 86_400;

function chars(s) {
  return Array.from(s).length;
}

function checkText(s, what, max, required) {
  assert.equal(typeof s, "string", `${what} is a string`);
  const t = s.trim();
  if (required) assert.notEqual(t, "", `${what} is empty`);
  assert.ok(chars(t) <= max, `${what} is longer than ${max} characters`);
  assert.ok(!/\p{Cc}/u.test(t), `${what} has a control character`);
}

function checkUrl(url) {
  assert.equal(typeof url, "string");
  assert.ok(url.startsWith("https://"), "a link is not https");
  assert.ok(url.length <= MAX.url && !/[\s\p{Cc}"'<>\\`]/u.test(url), "a link has a forbidden character or is too long");
  const authority = url.slice("https://".length).split(/[/?#]/)[0];
  assert.ok(authority !== "" && !authority.includes("@") && /^[A-Za-z0-9]/.test(authority), "a link has a bad host");
}

function only(o, allowed, what) {
  assert.ok(o !== null && typeof o === "object" && !Array.isArray(o), `${what} is an object`);
  for (const k of Object.keys(o)) assert.ok(allowed.includes(k), `${what} has an unknown field ${k}`);
}

function checkTime(t, what, now) {
  assert.ok(Number.isInteger(t), `${what} is an integer`);
  assert.ok(t >= MIN_TIME && t <= now + DAY, `${what} is not a recent Unix time`);
}

// Throws on anything the project site would refuse. `now` is when the page is built.
function validateFeed(text, now) {
  assert.ok(new TextEncoder().encode(text).length <= 64 * 1024, "the document is too large");
  const doc = JSON.parse(text);
  only(doc, ["schema", "updated", "sections"], "the document");
  assert.equal(doc.schema, 1);
  if (doc.updated !== undefined) checkTime(doc.updated, "updated", now);
  assert.ok(Array.isArray(doc.sections), "sections is an array");
  assert.ok(doc.sections.length <= MAX.sections, "more than 8 sections");
  for (const s of doc.sections) {
    only(s, ["title", "updated", "stale_after_secs", "link", "figures", "series"], "a section");
    checkText(s.title, "a section title", MAX.title, true);
    if (s.updated !== undefined) checkTime(s.updated, "a section's updated", now);
    if (s.stale_after_secs !== undefined) {
      assert.ok(Number.isInteger(s.stale_after_secs) && s.stale_after_secs >= 60 && s.stale_after_secs <= 365 * DAY, "stale_after_secs is out of range");
    }
    if (s.link !== undefined) {
      only(s.link, ["label", "url"], "a link");
      checkText(s.link.label, "a link label", MAX.label, true);
      checkUrl(s.link.url);
    }
    const figures = s.figures ?? [];
    assert.ok(Array.isArray(figures) && figures.length <= MAX.figures, "more than 12 figures in a section");
    for (const f of figures) {
      only(f, ["label", "value", "unit", "note"], "a figure");
      checkText(f.label, "a figure label", MAX.label, true);
      const v = f.value;
      if (v !== undefined && v !== null) {
        if (typeof v === "number") assert.ok(Number.isFinite(v) && Math.abs(v) <= 1e18, "a figure value is out of range");
        else {
          assert.equal(typeof v, "string", "a figure value is not a number, a string or null");
          checkText(v, "a figure value", MAX.value, false);
        }
      }
      if (f.unit !== undefined) checkText(f.unit, "a unit", MAX.unit, false);
      if (f.note !== undefined) checkText(f.note, "a note", MAX.note, false);
    }
    const series = s.series ?? [];
    assert.ok(Array.isArray(series) && series.length <= MAX.series, "more than 3 series in a section");
    for (const r of series) {
      only(r, ["title", "unit", "kind", "start", "interval_secs", "values"], "a series");
      checkText(r.title, "a series title", MAX.title, true);
      if (r.unit !== undefined) checkText(r.unit, "a unit", MAX.unit, false);
      if (r.kind !== undefined) assert.ok(["line", "bars", "area"].includes(r.kind), "a series kind is not line, bars or area");
      assert.ok(Number.isInteger(r.start) && Number.isInteger(r.interval_secs), "start and interval_secs are integers");
      assert.ok(r.interval_secs >= 60 && r.interval_secs <= 31 * DAY, "interval_secs is out of range");
      assert.ok(Array.isArray(r.values) && r.values.length >= 1 && r.values.length <= MAX.points, "a series needs 1 to 200 values");
      const last = r.start + r.interval_secs * (r.values.length - 1);
      assert.ok(r.start >= MIN_TIME && last <= now + DAY, "a series does not end in the past");
      for (const v of r.values) {
        if (v === null) continue;
        assert.ok(typeof v === "number" && Number.isFinite(v) && Math.abs(v) <= 1e18, "a series value is out of range");
        assert.ok(v >= 0, "a series value is negative");
      }
    }
  }
  return doc;
}

const refused = (doc, now = NOW) => assert.throws(() => validateFeed(JSON.stringify(doc), now));
const section = (extra = {}) => ({ title: "T", ...extra });

test("the validator accepts the schema's own example and refuses what the project site refuses", () => {
  const ok = {
    schema: 1,
    updated: NOW - 60,
    sections: [
      {
        title: "Corpus",
        updated: NOW - 60,
        stale_after_secs: 3600,
        link: { label: "Full dashboard", url: "https://example.org/dashboard" },
        figures: [
          { label: "Pages fetched", value: 1234567, unit: "pages", note: "since June" },
          { label: "Accuracy", value: null, note: "not measured yet" },
          { label: "Mode", value: "catching up" },
        ],
        series: [{ title: "Per day", unit: "pages", kind: "bars", start: NOW - 4 * DAY, interval_secs: DAY, values: [1200, 1500, null, 1800] }],
      },
    ],
  };
  validateFeed(JSON.stringify(ok), NOW);
  const long = (n) => "x".repeat(n);
  const fig = (f) => section({ figures: [f] });
  const ser = (r) => section({ series: [{ title: "S", start: NOW - DAY, interval_secs: 3600, values: [1], ...r }] });
  const bad = [
    { schema: 2, sections: [] },
    { schema: 1 },
    { schema: 1, sections: [], extra: 1 },
    { schema: 1, updated: 5, sections: [] },
    { schema: 1, updated: NOW + 2 * DAY, sections: [] },
    { schema: 1, sections: Array(9).fill(section()) },
    { schema: 1, sections: [section({ title: "" })] },
    { schema: 1, sections: [section({ title: long(81) })] },
    { schema: 1, sections: [section({ title: "a\u0007b" })] },
    { schema: 1, sections: [section({ extra: 1 })] },
    { schema: 1, sections: [section({ stale_after_secs: 59 })] },
    { schema: 1, sections: [section({ stale_after_secs: 366 * DAY })] },
    { schema: 1, sections: [section({ link: { label: "x", url: "http://example.org/" } })] },
    { schema: 1, sections: [section({ link: { label: "x", url: "https://user:pw@example.org/" } })] },
    { schema: 1, sections: [section({ link: { label: long(61), url: "https://example.org/" } })] },
    { schema: 1, sections: [section({ link: { label: "x", url: "https://example.org/" + long(300) } })] },
    { schema: 1, sections: [section({ link: { label: "x", url: "https://example.org/", extra: 1 } })] },
    { schema: 1, sections: [section({ figures: Array(13).fill({ label: "A" }) })] },
    { schema: 1, sections: [fig({ label: long(61) })] },
    { schema: 1, sections: [fig({ label: "A", value: [1] })] },
    { schema: 1, sections: [fig({ label: "A", value: 1e30 })] },
    { schema: 1, sections: [fig({ label: "A", value: long(41) })] },
    { schema: 1, sections: [fig({ label: "A", unit: long(25) })] },
    { schema: 1, sections: [fig({ label: "A", note: long(161) })] },
    { schema: 1, sections: [fig({ label: "A", extra: 1 })] },
    { schema: 1, sections: [section({ series: Array(4).fill({ title: "S", start: NOW - DAY, interval_secs: 60, values: [1] }) })] },
    { schema: 1, sections: [ser({ values: [] })] },
    { schema: 1, sections: [ser({ values: Array(201).fill(1) })] },
    { schema: 1, sections: [ser({ values: [-1] })] },
    { schema: 1, sections: [ser({ interval_secs: 59 })] },
    { schema: 1, sections: [ser({ interval_secs: 32 * DAY })] },
    { schema: 1, sections: [ser({ kind: "dots" })] },
    { schema: 1, sections: [ser({ start: 5 })] },
    { schema: 1, sections: [ser({ start: NOW + 2 * DAY })] },
    { schema: 1, sections: [ser({ start: NOW - DAY, interval_secs: DAY, values: [1, 2, 3, 4] })] },
    { schema: 1, sections: [ser({ start: 1.5 })] },
  ];
  for (const doc of bad) refused(doc);
  assert.throws(() => validateFeed("{" + " ".repeat(65 * 1024) + "}", NOW), /too large/);
});

// ---------- the mapping, on the trimmed fixtures ----------

const figures = (s) => Object.fromEntries(s.figures.map((f) => [f.label, f]));
const card = (doc, title) => doc.sections.find((s) => s.title === title);

test("the fixtures map to a feed the project site accepts", () => {
  const text = JSON.stringify(buildFeed(pipeline(), view(), NOW));
  const doc = validateFeed(text, NOW);
  assert.deepEqual(
    doc.sections.map((s) => s.title),
    ["Jobs in flight", "Pipeline", "What's next", "What's been done", "Volunteers", "Throughput"],
  );
  for (const s of doc.sections) {
    assert.deepEqual(s.link, { label: "Full dashboard", url: "https://ocr.usnewsmap.com/" });
    assert.ok(s.updated <= NOW && s.updated > NOW - 3 * 3600, `${s.title} updated`);
  }
  assert.ok(text.length < 12 * 1024, "small");
  // None of the figures is made up: all of them have a value in the fixtures.
  for (const s of doc.sections) for (const f of s.figures) assert.notEqual(f.value, null, `${s.title}: ${f.label}`);
});

test("jobs in flight are the BOINC replica counts, with the dashboard's labels", () => {
  const s = card(buildFeed(pipeline(), view(), NOW), "Jobs in flight");
  const f = figures(s);
  assert.equal(f["BOINC unsent result replicas"].value, 400);
  assert.equal(f["BOINC assigned result replicas"].value, 150);
  assert.equal(f["usnm_ocr: unsent replicas"].value, 60);
  assert.equal(f["usnm_qwen: assigned replicas"].value, 80);
  assert.equal(f["usnm_gold: unsent replicas"].value, 0, "a real 0 stays 0");
  assert.equal(f["usnm_quality: assigned replicas"].value, 30);
  assert.equal(f["BOINC unsent result replicas"].note, "Result replicas, not pages or GPU eligibility.");
  assert.equal(f["BOINC assigned result replicas"].note, "Assigned does not mean actively computing.");
  assert.equal(f["usnm_ocr: unsent replicas"].note, undefined);
  // The oldest of the stages' own observation times.
  assert.equal(s.updated, Math.floor(Date.parse("2026-03-14T11:54:00Z") / 1000));
});

test("the pipeline lists the stages in order, and a 14-day window moves to the unit", () => {
  const s = card(buildFeed(pipeline(), view(), NOW), "Pipeline");
  assert.deepEqual(
    s.figures.map((f) => [f.label, f.value, f.unit]),
    [
      ["Pages in the ocr-bulk queue", 15000, "pages"],
      ["OCR pages at BOINC or awaiting collection", 800, "pages"],
      ["BOINC OCR workunits with a canonical result", 12000, "workunits, last 14 days"],
      ["OCR pages collected through hand-off checks", 9000, "pages, last 14 days"],
      ["OCR pages in committed imports", 7000, "pages, last 14 days"],
      ["OCR parts rejected or dead-lettered", 5, "parts, last 14 days"],
    ],
  );
  assert.equal(s.updated, Math.floor(Date.parse("2026-03-14T11:00:00Z") / 1000));
  assert.equal(s.stale_after_secs, 10800);
  assert.match(s.figures[2].note, /counts workunits/);
  assert.equal(s.figures[5].note, "Attempts, not pages lost.");
});

test("what's next gives the fetch queue's figures and the pages-left arithmetic the dashboard shows", () => {
  const v = view();
  const s = card(buildFeed(pipeline(), v, NOW), "What's next");
  const f = figures(s);
  assert.equal(f["Pages still to fetch"].value, v.fetch.queue_summary.pages_left);
  // The fixture's short queue: queued 3 quality manifests, 1 ocr, the fetching and cooling ones.
  const left = (m) => m.pages - m.fetched;
  const by = (kind) =>
    v.fetch.queue.filter((m) => ["queued", "fetching", "cooling"].includes(m.state) && m.kind === kind).reduce((n, m) => n + left(m), 0);
  assert.equal(f["Pages still to fetch for our OCR"].value, by("ocr"));
  assert.equal(f["Pages still to fetch for the quality checks"].value, by("quality"));
  assert.ok(by("ocr") > 0 && by("quality") > 0);
  assert.equal(f["Days to fetch the rest at the last day's pace"].value, Math.ceil(v.fetch.queue_summary.pages_left / v.fetch.last_day.pages));
  assert.equal(s.updated, Math.floor(Date.parse(v.generated_at) / 1000));
});

test("done and volunteers give the dashboard's own sums", () => {
  const v = view();
  const doc = buildFeed(pipeline(), v, NOW);
  const d = figures(card(doc, "What's been done"));
  assert.equal(d["Page scans fetched so far"].value, 500000);
  assert.equal(d["Pages searchable with our OCR"].value, 20000);
  assert.equal(d["Quality and Qwen pages loaded"].value, 500000);
  assert.equal(d["Quality and Qwen pages loaded"].unit, "pages, last 14 days");

  const buckets = v.boinc_week.week.buckets.filter((b) => !b.partial).slice(-8);
  const finished = buckets.reduce((n, b) => n + Object.values(b.completed).reduce((m, x) => m + x, 0), 0);
  const last = v.boinc_week.week.buckets.filter((b) => !b.partial).at(-1);
  const w = figures(card(doc, "Volunteers"));
  assert.equal(w["Volunteers active in the last 3 hours"].value, last.volunteers);
  assert.equal(w["Jobs finished in the last 24 hours"].value, finished);
  const mix = Object.entries(w).filter(([, f]) => f.unit === "last 24 hours");
  assert.equal(
    mix.reduce((n, [, f]) => n + f.value, 0),
    finished,
    "the job mix adds up to the jobs finished",
  );
  // Most jobs first, and the long phrase is shortened.
  assert.ok(mix.every(([label]) => label.length <= 60 && label.startsWith("Jobs ")));
  assert.ok(mix.map(([, f]) => f.value).every((n, i, all) => i === 0 || all[i - 1] >= n));
});

test("gold and gold_native jobs are one line, as the dashboard counts them, and unknown apps are other jobs", () => {
  const v = view();
  v.boinc_week.week.buckets = [
    { completed: { usnm_gold: 2, usnm_gold_native: 3, usnm_surya: 4, usnm_unheard_of: 1 }, partial: false, volunteers: 7 },
  ];
  const w = figures(card(buildFeed(pipeline(), v, NOW), "Volunteers"));
  assert.equal(w["Jobs testing a volunteer's OCR against known text"].value, 5);
  assert.equal(w["Other jobs"].value, 5);
  assert.equal(w["Jobs finished in the last 24 hours"].value, 10);
});

test("throughput gives the last-24-hours table, the 7-day average and two daily series", () => {
  const v = view();
  const s = card(buildFeed(pipeline(), v, NOW), "Throughput");
  const f = figures(s);
  assert.equal(f["Scans fetched"].value, v.fetch.last_day.pages);
  assert.equal(f["OCR pages imported"].value, 400);
  assert.equal(f["Pages scored by the quality checks"].value, 20000);
  const days = v.throughput.steps.find((x) => x.id === "ocr_import").days.map((d) => d.value);
  assert.equal(f["OCR pages imported a day"].value, Math.round(days.reduce((a, b) => a + b, 0) / days.length));
  assert.equal(f["OCR pages imported a day"].unit, "7-day average");
  assert.equal(f["Results back in the last full hour"].value, 100);
  assert.equal(f["Valid results in the last full hour"].value, 80);
  assert.equal(s.series.length, 2);
  const start = Math.floor(Date.parse(v.throughput.days_start) / 1000);
  for (const r of s.series) {
    assert.equal(r.start, start);
    assert.equal(r.interval_secs, 86400);
    assert.equal(r.kind, "bars");
    assert.equal(r.values.length, 7);
  }
  assert.deepEqual(s.series[0].values, v.throughput.steps.find((x) => x.id === "scan_fetch").days.map((d) => d.value));
  assert.deepEqual(s.series[1].values, days);
});

// ---------- what is missing is unknown, never 0 ----------

const values = (s) => s.figures.map((f) => f.value);
const allNull = (s) => values(s).every((x) => x === null);

test("a missing pipeline snapshot leaves the stage figures unknown and keeps the other sections", () => {
  for (const missing of [null, {}, { snapshot: null, stage_freshness: [] }, { snapshot: { stages: "x" } }]) {
    const doc = buildFeed(missing, view(), NOW);
    validateFeed(JSON.stringify(doc), NOW);
    assert.equal(doc.sections.length, 6);
    assert.ok(allNull(card(doc, "Jobs in flight")) && allNull(card(doc, "Pipeline")));
    assert.equal(card(doc, "Jobs in flight").updated, undefined, "no observation time to claim");
    assert.equal(figures(card(doc, "What's been done"))["Pages searchable with our OCR"].value, null);
    assert.equal(figures(card(doc, "What's been done"))["Page scans fetched so far"].value, 500000);
    assert.equal(figures(card(doc, "What's next"))["Pages still to fetch"].value, 610000);
    assert.ok(card(doc, "Throughput").series.length === 2);
  }
});

test("a missing dashboard view leaves the fetch, volunteer and throughput figures unknown", () => {
  for (const missing of [null, {}, { fetch: null, boinc_week: null, throughput: null }]) {
    const doc = buildFeed(pipeline(), missing, NOW);
    validateFeed(JSON.stringify(doc), NOW);
    for (const title of ["What's next", "Volunteers", "Throughput"]) assert.ok(allNull(card(doc, title)), title);
    assert.equal(card(doc, "Throughput").series, undefined);
    assert.equal(figures(card(doc, "Pipeline"))["Pages in the ocr-bulk queue"].value, 15000);
    assert.equal(figures(card(doc, "What's been done"))["Pages searchable with our OCR"].value, 20000);
    assert.equal(figures(card(doc, "What's been done"))["Page scans fetched so far"].value, null);
  }
});

test("a stage the dashboard would not show is unknown: stale, historical, unobserved or without a count", () => {
  const cases = {
    stale: (p) => (p.stage_freshness.find((x) => x.id === "queued").stale = true),
    historical: (p) => (p.snapshot.stages.find((x) => x.id === "queued").historical = true),
    unknown: (p) => (p.snapshot.stages.find((x) => x.id === "queued").state = "unknown"),
    blocked: (p) => (p.snapshot.stages.find((x) => x.id === "queued").state = "blocked"),
    nocount: (p) => delete p.snapshot.stages.find((x) => x.id === "queued").count,
    nullcount: (p) => (p.snapshot.stages.find((x) => x.id === "queued").count = null),
    negative: (p) => (p.snapshot.stages.find((x) => x.id === "queued").count = -3),
    stringcount: (p) => (p.snapshot.stages.find((x) => x.id === "queued").count = "15000"),
    nofreshness: (p) => (p.stage_freshness = p.stage_freshness.filter((x) => x.id !== "queued")),
    nostage: (p) => (p.snapshot.stages = p.snapshot.stages.filter((x) => x.id !== "queued")),
  };
  for (const [name, edit] of Object.entries(cases)) {
    const p = pipeline();
    edit(p);
    const s = card(buildFeed(p, view(), NOW), "Pipeline");
    assert.equal(figures(s)["Pages in the ocr-bulk queue"].value, null, name);
    assert.equal(figures(s)["OCR pages at BOINC or awaiting collection"].value, 800, name);
  }
});

test("a 14-day total with fewer than all its hours reported is unknown", () => {
  const p = pipeline();
  const imported = p.snapshot.stages.find((x) => x.id === "imported");
  imported.detail = imported.detail.replace("336 reported", "335 reported");
  p.snapshot.stages.find((x) => x.id === "quality_loaded").detail = "Scores, not OCR acceptance.";
  const doc = buildFeed(p, view(), NOW);
  assert.equal(figures(card(doc, "Pipeline"))["OCR pages in committed imports"].value, null);
  assert.equal(figures(card(doc, "What's been done"))["Quality and Qwen pages loaded"].value, null);
  assert.equal(figures(card(doc, "Pipeline"))["OCR pages collected through hand-off checks"].value, 9000);
  validateFeed(JSON.stringify(doc), NOW);
});

test("a stage label from the dashboard is used, and one that does not fit is shortened", () => {
  const p = pipeline();
  p.snapshot.stages.find((x) => x.id === "queued").label = "Pages in the queue, as renamed";
  assert.equal(card(buildFeed(p, view(), NOW), "Pipeline").figures[0].label, "Pages in the queue, as renamed");
  p.snapshot.stages.find((x) => x.id === "queued").label = "A very long label that the dashboard might one day use for a stage, with details";
  const label = card(buildFeed(p, view(), NOW), "Pipeline").figures[0].label;
  assert.ok(Array.from(label).length <= 60 && label.endsWith("…"), label);
  validateFeed(JSON.stringify(buildFeed(p, view(), NOW)), NOW);
  assert.equal(shorten("short"), "short");
  assert.equal(shorten("x".repeat(70)).length, 60);
  assert.equal(shorten("a\u0007b  c"), "a b c");
});

test("the fetch figures are unknown while the dashboard warms up or cannot read the fetcher's records", () => {
  const edits = {
    warming: (v) => (v.warming = { since: "2026-03-14T11:59:00Z" }),
    index_error: (v) => (v.fetch.index_error = "unreadable"),
  };
  for (const [name, edit] of Object.entries(edits)) {
    const v = view();
    edit(v);
    const doc = buildFeed(pipeline(), v, NOW);
    assert.ok(allNull(card(doc, "What's next")), name);
    assert.equal(figures(card(doc, "What's been done"))["Page scans fetched so far"].value, null, name);
    assert.equal(figures(card(doc, "Throughput"))["Scans fetched"].value, null, name);
    assert.equal(figures(card(doc, "Throughput"))["OCR pages imported"].value, 400, name);
    // The scan fetch's daily bars count the same fetch index as its scalar, so they wait too.
    const bars = card(doc, "Throughput").series.map((r) => r.title);
    assert.deepEqual(bars, ["OCR pages imported a day, last 7 days"], name);
  }
});

test("the queue's figures need a configured queue without an error", () => {
  for (const edit of [(v) => (v.fetch.queue_configured = false), (v) => (v.fetch.queue_error = "down"), (v) => delete v.fetch.queue_summary]) {
    const v = view();
    edit(v);
    const s = card(buildFeed(pipeline(), v, NOW), "What's next");
    assert.ok(allNull(s));
  }
  const v = view();
  delete v.fetch.queue;
  const f = figures(card(buildFeed(pipeline(), v, NOW), "What's next"));
  assert.equal(f["Pages still to fetch"].value, 610000, "the total does not need the list");
  assert.equal(f["Pages still to fetch for our OCR"].value, null);
  assert.equal(f["Pages still to fetch for the quality checks"].value, null);
  v.fetch.queue = [{ state: "queued", kind: "ocr", pages: "many", fetched: 0 }];
  assert.equal(figures(card(buildFeed(pipeline(), v, NOW), "What's next"))["Pages still to fetch for our OCR"].value, null);
});

test("no pace, no days: a last day without fetches gives no estimate", () => {
  const v = view();
  v.fetch.last_day.pages = 0;
  const f = figures(card(buildFeed(pipeline(), v, NOW), "What's next"));
  assert.equal(f["Days to fetch the rest at the last day's pace"].value, null);
  assert.equal(f["Pages still to fetch"].value, 610000);
  assert.equal(figures(card(buildFeed(pipeline(), v, NOW), "Throughput"))["Scans fetched"].value, 0, "a real 0 stays 0");
});

test("volunteer figures are unknown when the week is stale, missing or empty", () => {
  const edits = [(v) => (v.boinc_week.stale = true), (v) => (v.boinc_week.week = null), (v) => (v.boinc_week.week.buckets = [])];
  for (const edit of edits) {
    const v = view();
    edit(v);
    const s = card(buildFeed(pipeline(), v, NOW), "Volunteers");
    assert.equal(s.figures.length, 2);
    assert.ok(allNull(s));
  }
});

test("a day not counted for all 24 hours is a gap in the series and out of the average", () => {
  const v = view();
  const days = v.throughput.steps.find((x) => x.id === "ocr_import").days;
  days[2].hours_known = 20;
  days[5].value = null;
  const s = card(buildFeed(pipeline(), v, NOW), "Throughput");
  const imported = s.series.find((r) => r.title.startsWith("OCR pages imported"));
  assert.deepEqual(imported.values.map((x) => x === null), [false, false, true, false, false, true, false]);
  const whole = days.filter((d) => d.hours_known === 24 && d.value !== null).map((d) => d.value);
  const average = figures(s)["OCR pages imported a day"];
  assert.equal(average.value, Math.round(whole.reduce((a, b) => a + b, 0) / whole.length));
  assert.equal(average.note, "Average of the 5 fully counted days of the last 7.");
  validateFeed(JSON.stringify(buildFeed(pipeline(), v, NOW)), NOW);
  // No day counted at all: unknown, and no chart.
  for (const d of days) d.hours_known = 3;
  const none = card(buildFeed(pipeline(), v, NOW), "Throughput");
  assert.equal(figures(none)["OCR pages imported a day"].value, null);
  assert.ok(!none.series.some((r) => r.title.startsWith("OCR pages imported")));
});

test("the dashboard's start-up warning about its throughput history does not hide the steps it still shows", () => {
  // throughput.error is set when the saved history could not be read at start and stays set while
  // new data comes in; the dashboard shows the warning above its steps. What was counted in full
  // is told by hours_known.
  const v = view();
  v.throughput.error = "throughput history unreadable; started empty";
  v.throughput.steps.find((x) => x.id === "ocr_import").days[0].hours_known = 5;
  const s = card(buildFeed(pipeline(), v, NOW), "Throughput");
  const f = figures(s);
  assert.equal(f["OCR pages imported"].value, 400);
  assert.equal(f["Results back in the last full hour"].value, 100);
  assert.equal(s.series.length, 2);
  assert.equal(s.series[1].values[0], null, "a day that was not counted in full is still a gap");
  assert.equal(f["OCR pages imported a day"].note, "Average of the 6 fully counted days of the last 7.");
});

test("a missing day start gives no charts, and no throughput gives no throughput figures", () => {
  const w = view();
  delete w.throughput.days_start;
  const s = card(buildFeed(pipeline(), w, NOW), "Throughput");
  assert.equal(s.series, undefined);
  assert.equal(figures(s)["OCR pages imported"].value, 400);
  const v = view();
  delete v.throughput;
  const none = card(buildFeed(pipeline(), v, NOW), "Throughput");
  assert.equal(figures(none)["Scans fetched"].value, v.fetch.last_day.pages, "the fetch's own 24 hours need no throughput");
  assert.equal(figures(none)["OCR pages imported"].value, null);
  assert.equal(none.series, undefined);
});

test("a source with no observation time does not erase the times the others have", () => {
  const v = view();
  v.throughput.sources[0].observed_at = null;
  v.throughput.sources[1].observed_at = "2026-03-14T08:00:00Z";
  let s = card(buildFeed(pipeline(), v, NOW), "Throughput");
  assert.equal(s.updated, Math.floor(Date.parse("2026-03-14T08:00:00Z") / 1000), "the oldest known time");
  for (const x of v.throughput.sources) x.observed_at = null;
  s = card(buildFeed(pipeline(), v, NOW), "Throughput");
  assert.equal(s.updated, Math.floor(Date.parse(v.generated_at) / 1000), "only the fetch's own time is left");
  validateFeed(JSON.stringify(buildFeed(pipeline(), v, NOW)), NOW);
});

test("a time the clock cannot have reached is no observation time, and the feed still validates", () => {
  const p = pipeline();
  for (const s of p.snapshot.stages) if (s.id.startsWith("boinc_")) s.observed_at = "2030-01-01T00:00:00Z";
  const doc = buildFeed(p, view(), NOW);
  assert.equal(card(doc, "Jobs in flight").updated, undefined);
  validateFeed(JSON.stringify(doc), NOW);
  const w = view();
  w.throughput.days_start = "2030-01-01T00:00:00Z";
  assert.ok(card(buildFeed(pipeline(), w, NOW), "Throughput").series.length === 2, "the check is the project site's to make");
  refused(buildFeed(pipeline(), w, NOW));
});

test("odd input does not throw", () => {
  const junk = [null, undefined, 0, "x", [], [1], { sections: 1 }, { snapshot: [], stage_freshness: {} }, { fetch: [] }];
  for (const a of junk) {
    for (const b of junk) {
      const doc = buildFeed(a, b, NOW);
      validateFeed(JSON.stringify(doc), NOW);
    }
  }
  const v = view();
  v.boinc_week.week.buckets = [null, 3, { completed: [] }, { completed: { usnm_ocr: "x" }, partial: false, volunteers: 1 }];
  validateFeed(JSON.stringify(buildFeed(pipeline(), v, NOW)), NOW);
});

// ---------- answering, fetching and caching ----------

// A fetch that answers from the fixtures and counts its calls.
function upstreamStub({ pipelineBody = pipeline(), viewBody = view(), delay = 0 } = {}) {
  const calls = [];
  const fetchImpl = async (url, init) => {
    calls.push({ url, init });
    if (delay) {
      await new Promise((resolve, reject) => {
        const timer = setTimeout(resolve, delay);
        init.signal.addEventListener("abort", () => {
          clearTimeout(timer);
          reject(init.signal.reason);
        });
      });
    }
    const body = url === PIPELINE_URL ? pipelineBody : url === VIEW_URL ? viewBody : undefined;
    if (body === undefined) throw new Error("unexpected address");
    if (body instanceof Response) return body;
    return new Response(typeof body === "string" ? body : JSON.stringify(body), { status: 200, headers: { "content-type": "application/json" } });
  };
  return { fetchImpl, calls };
}

function memoryCache() {
  const store = new Map();
  return {
    store,
    async match(request) {
      const hit = store.get(request.url);
      return hit ? hit.clone() : undefined;
    },
    async put(request, response) {
      store.set(request.url, response);
    },
  };
}

const ask = (handle, path = "/feed.json", init = {}) => handle(new Request(`https://feed.example.workers.dev${path}`, init));
const quiet = (t) => t.mock.method(console, "error", () => {});

test("GET /feed.json answers a valid document with the caching headers", async (t) => {
  quiet(t);
  const { fetchImpl, calls } = upstreamStub();
  const clock = { t: Date.parse(view().generated_at) + 10_000 };
  const handle = createHandler({ fetchImpl, cache: null, now: () => clock.t });
  const response = await ask(handle);
  assert.equal(response.status, 200);
  assert.equal(response.headers.get("content-type"), "application/json; charset=utf-8");
  assert.equal(response.headers.get("cache-control"), "public, max-age=60");
  assert.equal(response.headers.get("x-content-type-options"), "nosniff");
  assert.equal(response.headers.get("x-feed-built-at"), String(Math.floor(clock.t / 1000)));
  const text = await response.text();
  validateFeed(text, Math.floor(clock.t / 1000));
  assert.deepEqual(calls.map((c) => c.url).sort(), [PIPELINE_URL, VIEW_URL].sort());
});

test("upstream is asked with a short limit, no redirects and the edge cache told to keep it a minute", async (t) => {
  quiet(t);
  const { fetchImpl, calls } = upstreamStub();
  await ask(createHandler({ fetchImpl, cache: null }));
  assert.equal(calls.length, 2);
  for (const { init } of calls) {
    assert.equal(init.method, "GET");
    assert.equal(init.redirect, "manual");
    assert.ok(init.signal instanceof AbortSignal);
    assert.equal(init.cf.cacheEverything, true);
    assert.deepEqual(init.cf.cacheTtlByStatus, { "200-299": 60, "300-499": 0, "500-599": 10 });
    assert.deepEqual(Object.keys(init.headers), ["accept"], "nothing is sent but the request");
  }
});

test("a minute of requests asks upstream once per endpoint; the next minute asks again", async (t) => {
  quiet(t);
  const { fetchImpl, calls } = upstreamStub();
  const clock = { t: Date.parse(view().generated_at) };
  const handle = createHandler({ fetchImpl, cache: null, now: () => clock.t });
  const first = await (await ask(handle)).text();
  for (let i = 0; i < 20; i++) {
    clock.t += 2000;
    assert.equal(await (await ask(handle)).text(), first);
  }
  assert.equal(calls.length, 2, "40 seconds, 21 requests");
  clock.t += 25_000; // 65 s after the first build
  await ask(handle);
  assert.equal(calls.length, 4);
});

test("requests at the same moment share one round of upstream requests", async (t) => {
  quiet(t);
  const { fetchImpl, calls } = upstreamStub({ delay: 5 });
  const handle = createHandler({ fetchImpl, cache: null });
  const all = await Promise.all(Array.from({ length: 10 }, () => ask(handle)));
  assert.ok(all.every((r) => r.status === 200));
  assert.equal(calls.length, 2);
});

test("the Cache API answer is used when there is one, and written when there is not", async (t) => {
  quiet(t);
  const cache = memoryCache();
  const { fetchImpl, calls } = upstreamStub();
  const clock = { t: Date.parse(view().generated_at) };
  const first = createHandler({ fetchImpl, cache, now: () => clock.t });
  const body = await (await ask(first)).text();
  assert.equal(cache.store.size, 1);
  assert.equal([...cache.store.keys()][0], "https://feed.example.workers.dev/feed.json");
  // Another isolate (a fresh handler) in the same location answers from the cache.
  const other = createHandler({ fetchImpl, cache, now: () => clock.t });
  assert.equal(await (await ask(other)).text(), body);
  assert.equal(calls.length, 2);
});

test("a cache that fails to write does not fail the answer", async (t) => {
  quiet(t);
  const cache = {
    async match() {
      return undefined;
    },
    async put() {
      throw new Error("no cache here");
    },
  };
  const response = await ask(createHandler({ fetchImpl: upstreamStub().fetchImpl, cache }));
  assert.equal(response.status, 200);
});

test("a cache that fails to read is a miss, and the feed is still built and served", async (t) => {
  quiet(t);
  const cache = {
    async match() {
      throw new Error("no cache here");
    },
    async put() {
      throw new Error("no cache here");
    },
  };
  const { fetchImpl, calls } = upstreamStub();
  const response = await ask(createHandler({ fetchImpl, cache }));
  assert.equal(response.status, 200);
  validateFeed(await response.text(), Math.floor(Date.now() / 1000));
  assert.equal(calls.length, 2);
});

test("a request whose cache lookup finishes after another built the feed does not ask upstream again", async (t) => {
  quiet(t);
  const { fetchImpl, calls } = upstreamStub();
  let release;
  const held = new Promise((resolve) => {
    release = resolve;
  });
  let lookups = 0;
  const cache = {
    async match() {
      lookups += 1;
      return lookups === 1 ? held : undefined;
    },
    async put() {},
  };
  const handle = createHandler({ fetchImpl, cache });
  const slow = ask(handle); // its lookup is held
  const quick = await ask(handle); // misses at once and builds
  assert.equal(quick.status, 200);
  assert.equal(calls.length, 2);
  release(undefined);
  const late = await slow;
  assert.equal(late.status, 200);
  assert.equal(calls.length, 2, "the late miss used the feed built meanwhile");
  assert.equal(await late.text(), await quick.text());
});

test("one endpoint down keeps the other's sections; both down is a 502 that is not remembered", async (t) => {
  quiet(t);
  const broken = [
    new Response("nope", { status: 503 }),
    new Response("{not json", { status: 200 }),
    new Response("{}", { status: 301, headers: { location: "https://elsewhere.example/" } }),
  ];
  for (const bad of broken) {
    const { fetchImpl } = upstreamStub({ viewBody: bad });
    const response = await ask(createHandler({ fetchImpl, cache: null }));
    assert.equal(response.status, 200);
    const doc = validateFeed(await response.text(), Math.floor(Date.now() / 1000));
    assert.equal(figures(card(doc, "Pipeline"))["Pages in the ocr-bulk queue"].value, 15000);
    assert.ok(allNull(card(doc, "What's next")));
  }
  const { fetchImpl } = upstreamStub({ pipelineBody: new Response("x", { status: 500 }), viewBody: new Response("x", { status: 500 }) });
  const clock = { t: Date.now() };
  const handle = createHandler({ fetchImpl, cache: null, now: () => clock.t });
  const down = await ask(handle);
  assert.equal(down.status, 502);
  assert.equal(down.headers.get("cache-control"), "public, max-age=10");
  assert.equal((await ask(handle)).status, 502);
  // It recovers on the next request.
  const up = createHandler({ fetchImpl: upstreamStub().fetchImpl, cache: null });
  assert.equal((await ask(up)).status, 200);
});

// Upstream that fails with a 500 until `state.up` is set, and counts its requests.
function flakyUpstream() {
  const state = { up: false, calls: 0 };
  const good = upstreamStub().fetchImpl;
  state.fetchImpl = async (url, init) => {
    state.calls += 1;
    return state.up ? good(url, init) : new Response("down", { status: 500 });
  };
  return state;
}

test("a double failure is remembered for ten seconds: one round per window, the 502 meanwhile", async (t) => {
  quiet(t);
  const up = flakyUpstream();
  const clock = { t: Date.parse(view().generated_at) };
  const handle = createHandler({ fetchImpl: up.fetchImpl, cache: null, now: () => clock.t });
  assert.equal((await ask(handle)).status, 502);
  assert.equal(up.calls, 2, "one round: both endpoints");
  for (let i = 0; i < 9; i++) {
    clock.t += 1000;
    const again = await ask(handle);
    assert.equal(again.status, 502);
    assert.equal(again.headers.get("cache-control"), "public, max-age=10");
  }
  assert.equal(up.calls, 2, "nine seconds of requests made no new round");
  clock.t += 1001; // ten seconds after the failure
  assert.equal((await ask(handle)).status, 502);
  assert.equal(up.calls, 4, "one round again once the window is over");
});

test("after a failure the next round after the window serves the recovered feed", async (t) => {
  quiet(t);
  const up = flakyUpstream();
  const clock = { t: Date.parse(view().generated_at) };
  const handle = createHandler({ fetchImpl: up.fetchImpl, cache: null, now: () => clock.t });
  assert.equal((await ask(handle)).status, 502);
  up.up = true;
  clock.t += 5000;
  assert.equal((await ask(handle)).status, 502, "inside the window the failure is still given");
  assert.equal(up.calls, 2);
  clock.t += 5001;
  const ok = await ask(handle);
  assert.equal(ok.status, 200, "no later than the window");
  assert.equal(ok.headers.get("cache-control"), "public, max-age=60");
  validateFeed(await ok.text(), Math.floor(clock.t / 1000));
  // The success is kept for a minute, and a failure after it is a new one.
  clock.t += 30_000;
  assert.equal((await ask(handle)).status, 200);
  assert.equal(up.calls, 4);
});

test("requests that arrive together during a failure share one round", async (t) => {
  quiet(t);
  const up = flakyUpstream();
  const handle = createHandler({ fetchImpl: up.fetchImpl, cache: null });
  const all = await Promise.all(Array.from({ length: 10 }, () => ask(handle)));
  assert.ok(all.every((r) => r.status === 502));
  assert.equal(up.calls, 2);
  assert.equal((await ask(handle)).status, 502);
  assert.equal(up.calls, 2, "and the failure is remembered for the ones after");
});

test("a failure is remembered when the cache lookup is slow too, and one endpoint up is a cached success", async (t) => {
  quiet(t);
  // A lookup that finishes after another request's failed round does not start another round.
  const up = flakyUpstream();
  let release;
  const held = new Promise((resolve) => {
    release = resolve;
  });
  let lookups = 0;
  const cache = {
    async match() {
      lookups += 1;
      return lookups === 1 ? held : undefined;
    },
    async put() {},
  };
  const handle = createHandler({ fetchImpl: up.fetchImpl, cache });
  const slow = ask(handle);
  assert.equal((await ask(handle)).status, 502);
  release(undefined);
  assert.equal((await slow).status, 502);
  assert.equal(up.calls, 2);
  // One endpoint up: a 200, kept for the full minute.
  const good = upstreamStub().fetchImpl;
  let calls = 0;
  const fetchImpl = async (url, init) => {
    calls += 1;
    return url === VIEW_URL ? new Response("down", { status: 503 }) : good(url, init);
  };
  const clock = { t: Date.parse(view().generated_at) };
  const partial = createHandler({ fetchImpl, cache: null, now: () => clock.t });
  assert.equal((await ask(partial)).status, 200);
  clock.t += 50_000;
  assert.equal((await ask(partial)).status, 200);
  assert.equal(calls, 2, "a partial result is cached for 60 seconds, not 10");
});

test("a slow upstream is given up on at the limit, and the answer keeps the other endpoint's figures", async (t) => {
  quiet(t);
  const slow = upstreamStub({ delay: 500 });
  const fast = upstreamStub();
  const fetchImpl = (url, init) => (url === VIEW_URL ? slow.fetchImpl(url, init) : fast.fetchImpl(url, init));
  const started = Date.now();
  const response = await ask(createHandler({ fetchImpl, cache: null, timeoutMs: 30 }));
  assert.ok(Date.now() - started < 400, "did not wait for the slow one");
  assert.equal(response.status, 200);
  const doc = JSON.parse(await response.text());
  assert.ok(allNull(card(doc, "Volunteers")));
  assert.equal(figures(card(doc, "Jobs in flight"))["BOINC unsent result replicas"].value, 400);
});

// A body that never says how long it is: `chunks` pieces of `size` bytes, counting the pieces read
// and whether the reader was cancelled.
function endlessBody(size, chunks) {
  const state = { pulled: 0, cancelled: false };
  const body = new ReadableStream({
    pull(controller) {
      if (state.pulled >= chunks) return controller.close();
      state.pulled += 1;
      controller.enqueue(new Uint8Array(size).fill(0x20));
    },
    cancel() {
      state.cancelled = true;
    },
  });
  return { state, response: new Response(body, { status: 200 }) };
}

test("a body with no length is read in pieces and cancelled once past the limit", async (t) => {
  quiet(t);
  const { state, response: big } = endlessBody(1024, 1000);
  const { fetchImpl } = upstreamStub({ viewBody: big });
  const response = await ask(createHandler({ fetchImpl, cache: null, maxBytes: 16 * 1024 }));
  assert.equal(response.status, 200);
  assert.ok(allNull(card(JSON.parse(await response.text()), "Volunteers")));
  assert.equal(state.cancelled, true);
  assert.ok(state.pulled < 20, `read ${state.pulled} pieces of 1000`);
});

test("the limit counts bytes, not characters", async (t) => {
  quiet(t);
  // A view with 5000 characters of padding that take 15000 bytes: under a 16 KiB limit by length, over it by size.
  const text = JSON.stringify({ ...view(), x: "\u20ac".repeat(5000) });
  assert.ok(text.length < 16 * 1024 && new TextEncoder().encode(text).length > 16 * 1024);
  const refusedView = upstreamStub({ viewBody: new Response(text, { status: 200 }) });
  const response = await ask(createHandler({ fetchImpl: refusedView.fetchImpl, cache: null, maxBytes: 16 * 1024 }));
  assert.ok(allNull(card(JSON.parse(await response.text()), "Volunteers")), "refused");
  // The same text is fine under a limit it fits.
  const accepted = upstreamStub({ viewBody: new Response(JSON.stringify({ ...view(), x: "\u20ac".repeat(30) }), { status: 200 }) });
  const ok = await ask(createHandler({ fetchImpl: accepted.fetchImpl, cache: null, maxBytes: 64 * 1024 }));
  assert.equal(figures(card(JSON.parse(await ok.text()), "Volunteers"))["Volunteers active in the last 3 hours"].value, 5);
});

test("an oversized upstream document is refused", async (t) => {
  quiet(t);
  const big = new Response("x".repeat(10), { status: 200, headers: { "content-length": String(5 * 1024 * 1024) } });
  const { fetchImpl } = upstreamStub({ viewBody: big });
  const response = await ask(createHandler({ fetchImpl, cache: null }));
  assert.equal(response.status, 200);
  assert.ok(allNull(card(JSON.parse(await response.text()), "Volunteers")));
});

test("other paths are 404 and other methods 405, with no upstream request", async (t) => {
  quiet(t);
  const { fetchImpl, calls } = upstreamStub();
  const handle = createHandler({ fetchImpl, cache: null });
  for (const path of ["/", "/feed", "/feed.json/", "/feed.json/x", "/api/pipeline.json", "/robots.txt"]) {
    assert.equal((await ask(handle, path)).status, 404, path);
  }
  for (const method of ["POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS"]) {
    const response = await ask(handle, "/feed.json", { method });
    assert.equal(response.status, 405, method);
    assert.equal(response.headers.get("allow"), "GET");
  }
  assert.equal((await ask(handle, "/feed.json?x=1")).status, 200, "a query string changes nothing");
  assert.equal(calls.length, 2);
});

test("the default export is a module Worker with only a fetch handler", async (t) => {
  assert.deepEqual(Object.keys(worker), ["fetch"]);
  t.mock.method(globalThis, "fetch", async () => new Response("down", { status: 503 }));
  quiet(t);
  globalThis.caches = { default: memoryCache() };
  const response = await worker.fetch(new Request("https://feed.example.workers.dev/feed.json"));
  assert.equal(response.status, 502);
  assert.equal((await worker.fetch(new Request("https://feed.example.workers.dev/other"))).status, 404);
  delete globalThis.caches;
});

test("the Worker reads only the dashboard's two public endpoints", () => {
  const source = readFileSync(new URL("./worker.mjs", import.meta.url), "utf8");
  const addresses = [...source.matchAll(/https?:\/\/[^\s"'`)]+/g)].map((m) => m[0]);
  assert.deepEqual([...new Set(addresses)].sort(), [
    "https://ocr.usnewsmap.com/",
    "https://ocr.usnewsmap.com/api/pipeline.json",
    "https://ocr.usnewsmap.com/api/view.json",
  ]);
  const code = source.replace(/^\s*\/\/.*$/gm, "");
  assert.ok(!/\benv\b|\.log\(|authorization|secret/i.test(code), "no bindings, secrets or logging of bodies");
  assert.ok(!/console\.error\([^)]*(body|text|url)/i.test(code), "a reason is logged, never an address or a body");
});
