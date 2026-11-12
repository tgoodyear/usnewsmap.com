// node --test edge/check/transcribe.test.mjs
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { DatabaseSync } from "node:sqlite";
import worker, { signSession } from "./worker.mjs";
import { cer, editDistance, normalizeReading, qualified, settleReadings, wantCalibration } from "./transcribe.mjs";

test("readings are normalised only in whitespace, bidi marks and controls", () => {
  assert.equal(normalizeReading("  א‏ב  \r\n\r\n ג  ד \n\n"), "אב\n\nג ד");
  assert.equal(normalizeReading("a\u0000b‮c"), "abc");
  assert.equal(normalizeReading("ſ́x"), "ſ́x"); // no NFC, no folding
});

test("edit distance and error rate count code points", () => {
  assert.equal(editDistance("kitten", "sitting"), 3);
  assert.equal(editDistance("😀a", "a"), 1);
  assert.equal(cer("abc", "abd"), 1 / 3);
  assert.equal(cer("", ""), 0);
});

const r = (contributor, text, at, unreadable = false) => ({ contributor, text, at, unreadable });

test("two matching readings settle; two differing ones wait for a third", () => {
  assert.deepEqual(settleReadings([r("a", "x", 1)]), { n: 1, settled: 0, outcome: null });
  assert.equal(settleReadings([r("a", "x", 1), r("b", "x", 2)]).outcome, "agreed");
  assert.deepEqual(settleReadings([r("a", "x", 1), r("b", "y", 2)]), { n: 2, settled: 0, outcome: "adjudicate" });
  const s = settleReadings([r("c", "z", 3), r("a", "x", 1), r("b", "y", 2)]);
  assert.deepEqual([s.outcome, s.text, s.method], ["adjudicated", "z", "adjudicated"]);
  assert.equal(settleReadings([r("a", "", 1, true), r("b", "", 2, true)]).unreadable, 1);
  assert.equal(settleReadings([r("a", "", 1, true), r("b", "x", 2)]).outcome, "adjudicate");
});

test("calibration gates who counts and when it is served", () => {
  assert.equal(qualified([], false), true);
  assert.equal(qualified([0], true), false);
  assert.equal(qualified([0.02, 0.05, 0.1], true), true);
  assert.equal(qualified([0.3, 0.3, 0.3], true), false);
  assert.equal(wantCalibration(0, 0, true), true);
  assert.equal(wantCalibration(3, 5, true), false);
  assert.equal(wantCalibration(3, 6, true), true);
  assert.equal(wantCalibration(0, 0, false), false);
});

// A D1 stand-in on node:sqlite, enough for these queries.
function fakeDb() {
  const db = new DatabaseSync(":memory:");
  db.exec(readFileSync(new URL("./schema.sql", import.meta.url), "utf8"));
  const stmt = (sql) => {
    let args = [];
    const o = {
      bind: (...a) => ((args = a), o),
      run: async () => ({ meta: { changes: Number(db.prepare(sql).run(...args).changes) } }),
      first: async () => db.prepare(sql).get(...args) ?? null,
      all: async () => ({ results: db.prepare(sql).all(...args).map((x) => ({ ...x })) }),
      sql, get args() { return args; },
    };
    return o;
  };
  return { prepare: stmt, batch: async (ss) => ss.map((s) => (db.prepare(s.sql).run(...s.args), { results: [] })), raw: db };
}

const env = (DB) => ({ ADMIN_TOKEN: "adm", SESSION_KEY: "sk", DB, ASSETS: { fetch: () => new Response("page") } });
const as = async (e, who, path, init = {}) =>
  worker.fetch(new Request("https://check.usnewsmap.com" + path, { ...init, headers: { ...(init.headers || {}), authorization: "Bearer " + (await signSession("sk", who)) } }), e, {});
const post = (e, who, path, obj) => as(e, who, path, { method: "POST", body: JSON.stringify(obj) });
const admin = (e, path, body, method = "POST") => worker.fetch(new Request("https://check.usnewsmap.com" + path, { method, body, headers: { authorization: "Bearer adm" } }), e, {});

async function setup(withCal) {
  const e = env(fakeDb());
  await admin(e, "/admin/languages/yi-truth", JSON.stringify({ name: "Yiddish", kind: "transcribe" }), "PUT");
  const tasks = [1, 2, 3].map((i) => JSON.stringify({ id: "yi-truth/s/l" + i, lang: "yi-truth", set: "s", kind: "transcribe", line: "x" + i + ".png", page: "p", label: "l" + i }));
  if (withCal) for (let i = 1; i <= 3; i++) tasks.push(JSON.stringify({ id: "yi-truth/s/cal" + i, lang: "yi-truth", set: "s", kind: "transcribe", line: "c" + i + ".png", truth: "abcdefghij" }));
  assert.equal((await admin(e, "/admin/tasks", tasks.join("\n"))).status, 200);
  return e;
}

test("a calibration task's truth never reaches the page or the export", async () => {
  const e = await setup(true);
  const seen = [];
  for (let i = 0; i < 3; i++) {
    const n = await (await as(e, "alice", "/api/next?lang=yi-truth&skip=" + seen.join(","))).json();
    assert.match(n.task.id, /cal/); // calibration comes first
    assert.equal(JSON.stringify(n).includes("abcdefghij"), false);
    assert.equal(n.task.calibration, undefined);
    seen.push(n.task.id);
    assert.equal((await post(e, "alice", "/api/readings", { task: n.task.id, text: "abcdefghij" })).status, 200);
  }
  const n = await (await as(e, "alice", "/api/next?lang=yi-truth")).json();
  assert.match(n.task.id, /\/l\d$/);
  const exp = await (await worker.fetch(new Request("https://check.usnewsmap.com/api/export?lang=yi-truth"), e, {})).text();
  assert.equal(exp.includes("cal"), false);
});

test("two readers who agree settle a task; a third adjudicates two who differ", async () => {
  const e = await setup(false);
  const id = "yi-truth/s/l1";
  assert.equal((await post(e, "a", "/api/readings", { task: id, text: "שלום" })).status, 200);
  assert.equal((await post(e, "b", "/api/readings", { task: id, text: "שלום  " })).status, 200);
  const done = e.DB.raw.prepare("SELECT settled, outcome FROM tasks WHERE id = ?").get(id);
  assert.deepEqual({ ...done }, { settled: 1, outcome: "agreed" });
  assert.equal(e.DB.raw.prepare("SELECT text FROM truths WHERE task_id = ?").get(id).text, "שלום");
  assert.equal((await post(e, "c", "/api/readings", { task: id, text: "x" })).status, 409);

  const id2 = "yi-truth/s/l2";
  await post(e, "a", "/api/readings", { task: id2, text: "one" });
  await post(e, "b", "/api/readings", { task: id2, text: "two" });
  assert.equal(e.DB.raw.prepare("SELECT outcome FROM tasks WHERE id = ?").get(id2).outcome, "adjudicate");
  // a and b have read it; c gets it, with the two human readings and no machine text
  const n = await (await as(e, "c", "/api/next?lang=yi-truth&skip=yi-truth/s/l3")).json();
  assert.equal(n.task.id, id2);
  assert.equal(n.task.stage, "adjudicate");
  assert.deepEqual([...n.task.candidates].sort(), ["one", "two"]);
  const again = await (await as(e, "a", "/api/next?lang=yi-truth&skip=yi-truth/s/l3")).json();
  assert.equal(again.task, null);
  await post(e, "c", "/api/readings", { task: id2, text: "one" });
  const t = e.DB.raw.prepare("SELECT outcome FROM tasks WHERE id = ?").get(id2);
  assert.equal(t.outcome, "adjudicated");

  const lines = (await (await worker.fetch(new Request("https://check.usnewsmap.com/api/export?lang=yi-truth"), e, {})).text()).trim().split("\n").map((l) => JSON.parse(l));
  const x = lines.find((l) => l.id === id2);
  assert.deepEqual([x.settled, x.method, x.text, x.readings.length], [true, "adjudicated", "one", 3]);
  assert.match(x.readings[0].reader, /^[0-9a-f]{10}$/);
  assert.equal(JSON.stringify(lines).includes('"a"'), false); // no session ids
});

test("someone who copies badly on calibration doesn't count", async () => {
  const e = await setup(true);
  for (const who of ["bad", "good1", "good2"]) for (let i = 1; i <= 3; i++) await post(e, who, "/api/readings", { task: "yi-truth/s/cal" + i, text: who === "bad" ? "zzzzzzzzzz" : "abcdefghij" });
  const id = "yi-truth/s/l1";
  await post(e, "bad", "/api/readings", { task: id, text: "q" });
  await post(e, "good1", "/api/readings", { task: id, text: "q" });
  assert.equal(e.DB.raw.prepare("SELECT outcome FROM tasks WHERE id = ?").get(id).outcome, null); // bad's reading doesn't count
  await post(e, "good2", "/api/readings", { task: id, text: "q" });
  assert.equal(e.DB.raw.prepare("SELECT outcome FROM tasks WHERE id = ?").get(id).outcome, "agreed");
});

test("readings need text or the nothing-legible flag, never both", async () => {
  const e = await setup(false);
  assert.equal((await post(e, "a", "/api/readings", { task: "yi-truth/s/l1", text: "" })).status, 400);
  assert.equal((await post(e, "a", "/api/readings", { task: "yi-truth/s/l1", text: "x", unreadable: true })).status, 400);
  assert.equal((await post(e, "a", "/api/readings", { task: "yi-truth/s/l1", text: "", unreadable: true })).status, 200);
  assert.equal((await post(e, "a", "/api/readings", { task: "nope", text: "x" })).status, 404);
});

test("a single-reader set settles on one reading and takes readings from the maintainer's session only", async () => {
  const e = env(fakeDb());
  await admin(e, "/admin/languages/de-968", JSON.stringify({ name: "German", kind: "transcribe", settle: "single", readers_prefix: "maintainer:", unlisted: true }), "PUT");
  await admin(e, "/admin/tasks", JSON.stringify({ id: "de-968/s/c1", lang: "de-968", set: "s", kind: "transcribe", line: "x.jpg" }));
  assert.equal((await post(e, "stranger", "/api/readings", { task: "de-968/s/c1", text: "x" })).status, 403);
  assert.equal((await as(e, "stranger", "/api/next?lang=de-968")).status, 403);
  const minted = await (await admin(e, "/admin/session", JSON.stringify({ contributor: "maintainer:tg" }))).json();
  assert.equal((await admin(e, "/admin/session", JSON.stringify({ contributor: "someone" }))).status, 400);
  const who = "maintainer:tg";
  assert.ok(minted.session);
  assert.equal((await post(e, who, "/api/readings", { task: "de-968/s/c1", text: "Die ſchöne Stadt" })).status, 200);
  assert.deepEqual({ ...e.DB.raw.prepare("SELECT settled, outcome FROM tasks WHERE id = 'de-968/s/c1'").get() }, { settled: 1, outcome: "single" });
  assert.equal(e.DB.raw.prepare("SELECT text FROM truths WHERE task_id = 'de-968/s/c1'").get().text, "Die ſchöne Stadt");
  const list = await (await worker.fetch(new Request("https://check.usnewsmap.com/api/languages"), e, {})).json();
  assert.deepEqual(list, []); // unlisted
});

test("tasks with a lower priority number are served first", async () => {
  const e = env(fakeDb());
  await admin(e, "/admin/languages/de-968", JSON.stringify({ name: "German", kind: "transcribe" }), "PUT");
  const mk = (id, priority) => JSON.stringify({ id: "de-968/s/" + id, lang: "de-968", set: "s", kind: "transcribe", line: id + ".jpg", priority });
  await admin(e, "/admin/tasks", [mk("late", 4), mk("first", 0), mk("mid", 2)].join("\n"));
  const seen = [];
  for (let i = 0; i < 3; i++) {
    const n = await (await as(e, "r", "/api/next?lang=de-968&skip=" + seen.join(","))).json();
    seen.push(n.task.id);
  }
  assert.deepEqual(seen, ["de-968/s/first", "de-968/s/mid", "de-968/s/late"]);
});
