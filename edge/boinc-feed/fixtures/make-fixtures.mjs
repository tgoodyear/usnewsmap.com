// Writes the two test fixtures, pipeline.json and view.json, into the directory given by --out.
// Usage: node make-fixtures.mjs --out <directory>
//
// The fixtures have the shape of the OCR dashboard's /api/pipeline.json and /api/view.json, cut
// down to what worker.mjs reads and a few of the fields around it. Every value is made up: round
// counts that are not the dashboard's figures, and times on one fixed day, 2026-03-14. Only field
// names, stage ids, app names and the dashboard's labels are the real ones.
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const args = process.argv.slice(2);
const usage = "usage: node make-fixtures.mjs --out <directory>";
if (args.length !== 2 || args[0] !== "--out" || !args[1]) {
  console.error(usage);
  process.exit(2);
}
const out = args[1];

const at = (hhmm, day = "2026-03-14") => `${day}T${hhmm}:00Z`;
const epoch = (iso) => Date.parse(iso) / 1000;
const HOUR = 3600;

// ---------- pipeline.json ----------

const LIVE = at("11:55");
const HOURLY = at("11:00");
const LAST_14 = "Last 336 hours, 336 reported. An example detail.";
// [id, label, count, observed_at, extras]
const stages = [
  ["scan_cached", "Queued pages fetched", 400000, LIVE],
  ["scan_planned", "Pages in fetch-queue manifests", 1000000, LIVE],
  ["scan_remaining", "Queued pages not yet fetched", 600000, LIVE],
  ["scan_total", "Scans fetched, all runs", 500000, LIVE],
  ["boinc_unsent", "BOINC unsent result replicas", 400, at("11:54")],
  ["boinc_in_progress", "BOINC assigned result replicas", 150, at("11:54")],
  ["boinc_usnm_ocr_unsent", "usnm_ocr: unsent replicas", 60, at("11:54")],
  ["boinc_usnm_ocr_in_progress", "usnm_ocr: assigned replicas", 20, at("11:54")],
  ["boinc_usnm_gold_unsent", "usnm_gold: unsent replicas", 0, at("11:54")],
  ["boinc_usnm_gold_in_progress", "usnm_gold: assigned replicas", 10, at("11:54")],
  ["boinc_usnm_quality_unsent", "usnm_quality: unsent replicas", 5, at("11:54")],
  ["boinc_usnm_quality_in_progress", "usnm_quality: assigned replicas", 30, at("11:54")],
  ["boinc_usnm_qwen_unsent", "usnm_qwen: unsent replicas", 300, at("11:54")],
  ["boinc_usnm_qwen_in_progress", "usnm_qwen: assigned replicas", 80, at("11:54")],
  ["boinc_usnm_ndlocr_unsent", "usnm_ndlocr: unsent replicas", 1, at("11:54")],
  ["boinc_usnm_ndlocr_in_progress", "usnm_ndlocr: assigned replicas", 0, at("11:54")],
  ["boinc_usnm_gold_native_unsent", "usnm_gold_native: unsent replicas", 0, at("11:54")],
  ["boinc_usnm_gold_native_in_progress", "usnm_gold_native: assigned replicas", 0, at("11:54")],
  ["boinc_usnm_ocr_native_unsent", "usnm_ocr_native: unsent replicas", 0, at("11:54")],
  ["boinc_usnm_ocr_native_in_progress", "usnm_ocr_native: assigned replicas", 0, at("11:54")],
  ["indexed", "Pages searchable with our OCR", 20000, LIVE],
  ["queued", "Pages in the ocr-bulk queue", 15000, HOURLY, { max_age_secs: 10800 }],
  ["allocated", "OCR pages at BOINC or awaiting collection", 800, HOURLY, { max_age_secs: 10800 }],
  ["validated", "BOINC OCR workunits with a canonical result, last 14 days", 12000, HOURLY, { max_age_secs: 10800 }],
  ["collected", "OCR pages collected through hand-off checks, last 14 days", 9000, HOURLY, { max_age_secs: 10800 }],
  ["imported", "OCR pages in committed imports, last 14 days", 7000, HOURLY, { max_age_secs: 10800 }],
  ["rejected", "OCR parts rejected or dead-lettered, last 14 days", 5, HOURLY, { max_age_secs: 10800 }],
  ["quality_loaded", "Quality and Qwen pages loaded, last 14 days", 500000, HOURLY, { max_age_secs: 10800 }],
  ["artifact_verified", "Cataloged verified locations", 1, at("00:00", "2026-03-01"), { historical: true }],
  ["run_history", "Cataloged engine runs", 1000, at("00:00", "2026-03-01"), { historical: true }],
  ["accepted", "Cataloged acceptance observations", 1, at("00:00", "2026-03-01"), { historical: true }],
];

const generated = Date.parse(at("12:00"));
const pipeline = {
  age_secs: 3600,
  last_error: null,
  retrieved_at: at("12:00"),
  stale: false,
  snapshot: {
    generated_at: LIVE,
    observed_at: HOURLY,
    producer: "example-catalog",
    schema_version: 1,
    stages: stages.map(([id, label, count, observed_at, extra = {}]) => ({
      count,
      detail: id === "validated" || id === "collected" || id === "imported" || id === "rejected" || id === "quality_loaded"
        ? LAST_14
        : "An example detail.",
      id,
      label,
      observed_at,
      state: "observed",
      ...extra,
    })),
  },
  stage_freshness: stages.map(([id, , , observed_at, extra = {}]) => ({
    age_secs: Math.round((generated - Date.parse(observed_at)) / 1000),
    historical: extra.historical === true,
    id,
    stale: false,
  })),
};

// ---------- view.json ----------

// A short fetch queue with one manifest in each state and kind; the real one lists them all.
const manifest = (name, kind, state, pages, fetched, marked_at = null) => ({
  error: null, fetched, kind, marked_at, name, pages, path: "", state,
});
const queue = [
  manifest("queue-example-1", "quality", "queued", 4000, 2000),
  manifest("queue-example-2", "quality", "queued", 1000, 500),
  manifest("queue-example-3", "quality", "queued", 1000, 0),
  manifest("queue-example-4", "ocr", "queued", 100, 0),
  manifest("queue-example-5", "quality", "fetched", 2000, 2000, at("00:00", "2026-03-10")),
  manifest("queue-example-6", "ocr", "fetched", 900, 900, at("00:00", "2026-03-09")),
  manifest("queue-example-7", "quality", "fetching", 1000, 400),
  manifest("queue-example-8", "ocr", "cooling", 250, 0),
];

// Ten 3-hour buckets ending at 12:00 on the day, the last one still running.
const firstBucket = epoch(at("06:00", "2026-03-13"));
const buckets = Array.from({ length: 10 }, (_, i) => ({
  completed: {
    usnm_ocr: 10 + i,
    usnm_quality: 5 + i,
    usnm_qwen: 100 + 10 * i,
    ...(i % 3 === 0 ? { usnm_gold: 4 } : {}),
  },
  hosts: 5 + (i % 3),
  partial: i === 9,
  queued: { usnm_ndlocr: 1, usnm_ocr: 20, usnm_qwen: 300, usnm_surya: 4 },
  start: firstBucket + i * 3 * HOUR,
  utc_offset_minutes: 0,
  volunteers: 3 + (i % 3),
}));

const days = (values) => values.map((value) => ({ hours_known: 24, value }));
const step = (id, label, unit, last_hour, last_day, values) => ({
  day_hours_known: 24, days: days(values), id, label, last_day, last_hour, unit,
});

const lastFullHour = epoch(at("11:00"));
const view = {
  generated_at: at("12:00"),
  warming: null,
  fetch: {
    activity: "queued",
    index_error: null,
    last_day: { bytes: 100000000000, mb_per_s: 1.2, pages: 24000, pages_per_hour: 1000 },
    last_hour: { bytes: 0, mb_per_s: 0, pages: 0, pages_per_hour: 0 },
    queue,
    queue_configured: true,
    queue_error: null,
    queue_summary: {
      cooling: 1, fetched: 40, fetching: 1, manifests: 200,
      pages: 1200000, pages_fetched: 590000, pages_left: 610000, queued: 158,
    },
    total: { bytes: 1000000000000, pages: 500000 },
  },
  boinc_week: {
    error: null,
    generated_at: at("11:56"),
    received: at("11:56"),
    stale: false,
    week: {
      bucket_hours: 3,
      buckets,
      generated_at: epoch(at("11:56")),
      generated_utc_offset_minutes: 0,
      schema: 1,
    },
  },
  throughput: {
    chart_start: new Date((epoch(at("12:00")) - 164 * HOUR) * 1000).toISOString().replace(".000", ""),
    days_start: new Date((lastFullHour - 167 * HOUR) * 1000).toISOString().replace(".000", ""),
    error: null,
    hours_start: new Date((lastFullHour - 23 * HOUR) * 1000).toISOString().replace(".000", ""),
    results_last_hour: 100,
    sources: [
      { name: "controller", observed_at: at("11:57"), stale: false },
      { name: "ship", observed_at: at("11:55"), stale: false },
      { name: "boinc", observed_at: at("11:53"), stale: false },
      { name: "batch", observed_at: at("11:58"), stale: false },
    ],
    steps: [
      step("scan_fetch", "Scan fetch", "pages", 1000, 24000, [20000, 22000, 25000, 18000, 26000, 30000, 24000]),
      step("ocr_import", "OCR pages imported", "pages", 20, 400, [1000, 1200, 900, 100, 300, 500, 400]),
      step("quality_load", "Quality results loaded", "pages", 800, 20000, [15000, 16000, 14000, 18000, 17000, 19000, 20000]),
    ],
    valid_last_hour: 80,
  },
};

mkdirSync(out, { recursive: true });
writeFileSync(join(out, "pipeline.json"), JSON.stringify(pipeline, null, 1) + "\n");
writeFileSync(join(out, "view.json"), JSON.stringify(view, null, 1) + "\n");
