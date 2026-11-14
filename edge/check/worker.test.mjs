// node --test edge/check/*.test.mjs
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import worker, { DISPLAY_REVISION, settle, signSession, verifySession } from "./worker.mjs";

test("a word settles at three answers with two thirds agreeing", () => {
  assert.deepEqual(settle({ a: 2, b: 0, neither: 0, unsure: 0 }), { n: 2, settled: 0, outcome: null });
  assert.deepEqual(settle({ a: 3, b: 0, neither: 0, unsure: 0 }), { n: 3, settled: 1, outcome: "a" });
  assert.deepEqual(settle({ a: 2, b: 1, neither: 0, unsure: 0 }), { n: 3, settled: 1, outcome: "a" });
  assert.deepEqual(settle({ a: 2, b: 1, neither: 1, unsure: 0 }), { n: 4, settled: 0, outcome: null });
  assert.deepEqual(settle({ a: 4, b: 2, neither: 0, unsure: 0 }), { n: 6, settled: 1, outcome: "a" });
});

test("not text settles like any answer, and ties go to the earlier choice", () => {
  assert.deepEqual(settle({ a: 1, b: 0, neither: 0, unsure: 0, notext: 2 }), { n: 3, settled: 1, outcome: "notext" });
  assert.deepEqual(settle({ a: 0, b: 0, neither: 0, unsure: 3, notext: 3 }), { n: 6, settled: 0, outcome: null });
});

test("a word that never agrees closes as disputed", () => {
  assert.deepEqual(settle({ a: 3, b: 3, neither: 1, unsure: 0 }), { n: 7, settled: 1, outcome: "disputed" });
});

test("a session round-trips and rejects tampering, other keys and expiry", async () => {
  const tok = await signSession("k1", "abc", 1000);
  assert.equal(await verifySession("k1", tok, 2000), "abc");
  assert.equal(await verifySession("k2", tok, 2000), null);
  assert.equal(await verifySession("k1", tok, 1000 + 31 * 86400e3), null);
  const [p, s] = tok.split(".");
  const forged = Buffer.from(JSON.stringify({ c: "other", e: 9e15 })).toString("base64url");
  assert.equal(await verifySession("k1", forged + "." + s, 2000), null);
  assert.equal(await verifySession("k1", p, 2000), null);
  assert.equal(await verifySession("k1", "x".repeat(500), 2000), null);
});

const env = { ADMIN_TOKEN: "secret-admin", SESSION_KEY: "sk", ASSETS: { fetch: () => new Response("page") } };
const call = (path, init = {}) => worker.fetch(new Request("https://check.usnewsmap.com" + path, init), env, {});

test("admin paths need the admin token", async () => {
  assert.equal((await call("/admin/tasks", { method: "POST", body: "" })).status, 401);
  assert.equal((await call("/admin/tasks", { method: "POST", body: "", headers: { authorization: "Bearer wrong" } })).status, 401);
  assert.equal((await call("/admin/tasks", { method: "POST", body: "", headers: { authorization: "Bearer secret-admin-x" } })).status, 401);
  assert.equal((await call("/admin/dump")).status, 401);
});

test("answers and next need a session", async () => {
  assert.equal((await call("/api/next?lang=chr")).status, 401);
  assert.equal((await call("/api/answers", { method: "POST", body: "{}" })).status, 401);
  const forged = await signSession("not-the-key", "abc");
  assert.equal((await call("/api/next?lang=chr", { headers: { authorization: "Bearer " + forged } })).status, 401);
});

test("clip keys can't escape the bucket", async () => {
  const seen = [];
  const e2 = { ...env, CLIPS: { get: async (k) => (seen.push(k), null) } };
  for (const p of ["/clips/a%2F..%2Fb", "/clips/" + "a".repeat(301), "/clips/a b"]) {
    const r = await worker.fetch(new Request("https://check.usnewsmap.com" + p), e2, {});
    assert.equal(r.status, 404, p);
  }
  assert.deepEqual(seen, []);
});

test("other paths are the page", async () => {
  assert.equal(await (await call("/chr")).text(), "page");
});

test("the SQL settling rule matches settle()", async () => {
  const { DatabaseSync } = await import("node:sqlite");
  const { SETTLE_SQL } = await import("./worker.mjs");
  const { readFileSync } = await import("node:fs");
  const db = new DatabaseSync(":memory:");
  db.exec(readFileSync(new URL("./schema.sql", import.meta.url), "utf8"));
  db.prepare("INSERT INTO tasks (id, lang, set_name, body) VALUES ('t', 'chr', 's', '{}')").run();
  const add = db.prepare("INSERT INTO answers (task_id, contributor, choice, at) VALUES ('t', ?, ?, 0)");
  const upd = db.prepare(SETTLE_SQL);
  const get = db.prepare("SELECT n, settled, outcome FROM tasks WHERE id = 't'");
  const kinds = ["a", "b", "neither", "unsure", "notext"];
  for (let code = 0; code < 6 ** kinds.length; code++) {
    const counts = {};
    let rest = code;
    for (const k of kinds) { counts[k] = rest % 6; rest = Math.floor(rest / 6); }
    db.exec("DELETE FROM answers");
    let i = 0;
    for (const k of kinds) for (let j = 0; j < counts[k]; j++) add.run("c" + i++, k);
    upd.run("t");
    assert.deepEqual({ ...get.get() }, settle(counts), JSON.stringify(counts));
  }
});

test("the dump has every table's rows", async () => {
  const data = { languages: [{ code: "chr", config: "{}" }], tasks: [{ id: "t1" }, { id: "t2" }], answers: [{ task_id: "t1", contributor: "c", choice: "a", at: 1 }],
    answers_excluded: [{ task_id: "t2", contributor: "d", choice: "b", at: 2 }] };
  let batches = 0;
  const DB = {
    prepare: (sql) => ({ sql }),
    batch: async (stmts) => { batches++; return stmts.map((st) => ({ results: data[st.sql.match(/FROM (\w+)/)[1]] })); },
  };
  const r = await worker.fetch(new Request("https://check.usnewsmap.com/admin/dump", { headers: { authorization: "Bearer secret-admin" } }), { ...env, DB }, {});
  assert.equal(r.status, 200);
  const lines = (await r.text()).trim().split("\n").map((l) => JSON.parse(l));
  assert.deepEqual(lines.map((l) => l.table), ["languages", "tasks", "tasks", "answers", "answers_excluded"]);
  assert.equal(lines[3].row.choice, "a");
  assert.equal(lines[4].row.choice, "b");
  assert.equal(batches, 1, "one batch, so one snapshot");
});

test("migration 0001 keeps every answer and allows notext", async () => {
  const { DatabaseSync } = await import("node:sqlite");
  const { readFileSync } = await import("node:fs");
  const db = new DatabaseSync(":memory:");
  db.exec("CREATE TABLE answers (task_id TEXT NOT NULL, contributor TEXT NOT NULL, choice TEXT NOT NULL CHECK (choice IN ('a', 'b', 'neither', 'unsure')), at INTEGER NOT NULL, PRIMARY KEY (task_id, contributor)); CREATE INDEX answers_by_contributor ON answers (contributor);");
  const add = db.prepare("INSERT INTO answers (task_id, contributor, choice, at) VALUES (?, ?, ?, ?)");
  assert.throws(() => add.run("t", "x", "notext", 0));
  ["a", "b", "neither", "unsure"].forEach((c, i) => add.run("t" + i, "c" + i, c, i));
  db.exec(readFileSync(new URL("./migrations/0001-answers-notext.sql", import.meta.url), "utf8"));
  assert.equal(db.prepare("SELECT COUNT(*) AS n FROM answers").get().n, 4);
  add.run("t9", "c9", "notext", 9);
  assert.throws(() => add.run("t9", "c9", "a", 9)); // the primary key survived
  assert.equal(db.prepare("SELECT COUNT(*) AS n FROM sqlite_master WHERE name = 'answers_by_contributor'").get().n, 1);
});

test("an oversized body is refused while streaming, before it is buffered", async () => {
  const { readLimited } = await import("./worker.mjs");
  let pulled = 0;
  const stream = new ReadableStream({
    pull(c) { pulled++; c.enqueue(new Uint8Array(1024)); if (pulled > 100) c.close(); },
  });
  const req = new Request("https://check.usnewsmap.com/api/session", { method: "POST", body: stream, duplex: "half" });
  await assert.rejects(readLimited(req, 4096), (e) => e.status === 413);
  assert.ok(pulled <= 6, `read ${pulled} chunks`);
  const declared = new Request("https://check.usnewsmap.com/api/session", { method: "POST", body: "x", headers: { "content-length": "999999" } });
  await assert.rejects(readLimited(declared, 4096), (e) => e.status === 413);
  assert.equal(await readLimited(new Request("https://x/", { method: "POST", body: '{"a":1}' }), 4096), '{"a":1}');
  const r = await call("/api/session", { method: "POST", body: "x".repeat(5000) });
  assert.equal(r.status, 413);
});

test("a language update that keeps examples keeps the stored ones, read in the same statement", async () => {
  const { DatabaseSync } = await import("node:sqlite");
  const { KEEP_EXAMPLES_SQL } = await import("./worker.mjs");
  const { readFileSync } = await import("node:fs");
  const db = new DatabaseSync(":memory:");
  db.exec(readFileSync(new URL("./schema.sql", import.meta.url), "utf8"));
  const put = db.prepare("INSERT INTO languages (code, config) VALUES (?, ?)");
  put.run("chr", JSON.stringify({ examples: { "Ꭰ": ["chr/new.png"] }, intro: "old", name: "Cherokee" }));
  put.run("sr", JSON.stringify({ intro: "old", name: "Serbian" }));
  const upd = db.prepare(KEEP_EXAMPLES_SQL);
  // The body carries an older copy of the examples: the stored ones win.
  assert.equal(upd.run(JSON.stringify({ examples: { "Ꭰ": ["chr/old.png"] }, intro: "new", name: "Cherokee" }), "chr").changes, 1);
  assert.equal(upd.run(JSON.stringify({ intro: "new", name: "Serbian" }), "sr").changes, 1);
  assert.equal(upd.run(JSON.stringify({ name: "Yiddish" }), "yi").changes, 0);
  const get = (code) => JSON.parse(db.prepare("SELECT config FROM languages WHERE code = ?").get(code).config);
  assert.deepEqual(get("chr"), { examples: { "Ꭰ": ["chr/new.png"] }, intro: "new", name: "Cherokee" });
  assert.deepEqual(get("sr"), { intro: "new", name: "Serbian" });
});

test("a blind task's private fields stay off /api/next, and off /api/export until it settles", async () => {
  const open = { page: "p", a: "tbe", b: "the", line: "l", word: "w", private: { a_is: "loc", b_is: "xiaomi", truth: "b", strata: { decade: 1890 } } };
  const plain = { page: "p", a: "x", b: "y", a_is: "loc", b_is: "ours" };
  const rows = [
    { id: "en/s/1", set_name: "s", body: JSON.stringify(open), outcome: null, settled: 0, a: 0, b: 1 },
    { id: "en/s/2", set_name: "s", body: JSON.stringify(open), outcome: "b", settled: 1, a: 0, b: 3 },
    { id: "chr/s/3", set_name: "s", body: JSON.stringify(plain), outcome: null, settled: 0, a: 1, b: 0 },
  ];
  const DB = { prepare: (sql) => ({ bind: () => ({ first: async () => (sql.includes("FROM tasks t WHERE lang") ? { id: "en/s/1", body: rows[0].body, n: 1 } : { k: 0 }), all: async () => ({ results: sql.includes("answers_excluded") ? [] : rows }) }), all: async () => ({ results: rows }) }) };
  const token = await signSession(env.SESSION_KEY, "c1");
  const next = await worker.fetch(new Request("https://check.usnewsmap.com/api/next?lang=en", { headers: { authorization: "Bearer " + token } }), { ...env, DB }, {});
  const task = (await next.json()).task;
  assert.equal(task.a, "tbe");
  assert.equal(JSON.stringify(task).includes("xiaomi"), false);
  assert.equal("private" in task, false);
  const exp = await worker.fetch(new Request("https://check.usnewsmap.com/api/export?lang=en"), { ...env, DB }, {});
  const lines = (await exp.text()).trim().split("\n").map((l) => JSON.parse(l));
  assert.equal(lines[0].a_is, undefined);
  assert.equal(lines[0].truth, undefined);
  assert.deepEqual(lines[0].strata, { decade: 1890 });
  assert.equal(lines[1].b_is, "xiaomi");
  assert.equal(lines[1].truth, "b");
  // a task with no `private` exports as before
  assert.equal(lines[2].a_is, "loc");
  assert.equal("strata" in lines[2], false);
});

test("a settled blind task takes no more answers, and other tasks still take revisions", async () => {
  const { DatabaseSync } = await import("node:sqlite");
  const { ANSWER_SQL, SETTLE_SQL } = await import("./worker.mjs");
  const { readFileSync } = await import("node:fs");
  const db = new DatabaseSync(":memory:");
  db.exec(readFileSync(new URL("./schema.sql", import.meta.url), "utf8"));
  db.prepare("INSERT INTO tasks (id, lang, set_name, body) VALUES ('blind', 'en', 's', ?), ('plain', 'chr', 's', '{}')").run(JSON.stringify({ private: { a_is: "loc" } }));
  const answer = (task, who, choice) => { db.prepare(ANSWER_SQL).run(task, who, choice, 0); db.prepare(SETTLE_SQL).run(task); };
  const row = (id) => ({ ...db.prepare("SELECT n, settled, outcome FROM tasks WHERE id = ?").get(id) });
  for (const id of ["blind", "plain"]) { answer(id, "c1", "a"); answer(id, "c2", "a"); answer(id, "c3", "b"); }
  assert.deepEqual(row("blind"), { n: 3, settled: 1, outcome: "a" });
  for (const id of ["blind", "plain"]) answer(id, "c1", "unsure");
  assert.deepEqual(row("blind"), { n: 3, settled: 1, outcome: "a" }, "the blind task stays settled");
  assert.equal(db.prepare("SELECT choice FROM answers WHERE task_id = 'blind' AND contributor = 'c1'").get().choice, "a");
  assert.deepEqual(row("plain"), { n: 3, settled: 0, outcome: null }, "other tasks reopen as before");
});

test("answers records nothing and answers 409 when the conditional write saves no row", async () => {
  const DB = {
    prepare: (sql) => ({ sql, bind() { return this; }, first: async () => ({ id: "t", settled: 0, blind: "object" }) }),
    batch: async () => [{ meta: { changes: 0 } }, { meta: { changes: 1 } }],
  };
  const token = await signSession(env.SESSION_KEY, "c1");
  const r = await worker.fetch(new Request("https://check.usnewsmap.com/api/answers", { method: "POST", headers: { authorization: "Bearer " + token }, body: JSON.stringify({ task: "t", choice: "a" }) }), { ...env, DB }, {});
  assert.equal(r.status, 409);
});

// ---------- words whose readings look alike (#1939) ----------

// D1's calls on node:sqlite, with the real schema, so the Worker's own SQL runs end to end.
async function sqliteD1() {
  const { DatabaseSync } = await import("node:sqlite");
  const { readFileSync } = await import("node:fs");
  const db = new DatabaseSync(":memory:");
  db.exec(readFileSync(new URL("./schema.sql", import.meta.url), "utf8"));
  const isRead = (sql) => /^\s*(SELECT|WITH)/i.test(sql) && !/^\s*WITH[\s\S]*\bUPDATE\b/i.test(sql);
  const make = (sql, args = []) => ({
    sql,
    bind: (...a) => make(sql, a),
    first: async () => { const r = db.prepare(sql).get(...args); return r ? { ...r } : null; },
    all: async () => ({ results: db.prepare(sql).all(...args).map((r) => ({ ...r })) }),
    run: async () => ({ meta: { changes: Number(db.prepare(sql).run(...args).changes) } }),
  });
  const DB = {
    prepare: (sql) => make(sql),
    batch: async (stmts) => {
      db.exec("BEGIN");
      try {
        const out = [];
        for (const st of stmts) out.push(isRead(st.sql) ? await st.all() : await st.run());
        db.exec("COMMIT");
        return out;
      } catch (e) { db.exec("ROLLBACK"); throw e; }
    },
  };
  return { db, DB };
}

const adminCall = (DB, path, body, raw) => worker.fetch(new Request("https://check.usnewsmap.com" + path, { method: "POST", headers: { authorization: "Bearer secret-admin" }, body: raw ?? JSON.stringify(body) }), { ...env, DB }, {});

test("marking a task confusable_display sets its answers aside, keeps them, and holds it until reopened", async () => {
  const { db, DB } = await sqliteD1();
  const E = { ...env, DB };
  const get = async (path, token) => worker.fetch(new Request("https://check.usnewsmap.com" + path, { headers: token ? { authorization: "Bearer " + token } : {} }), E, {});
  const post = async (path, token, body) => worker.fetch(new Request("https://check.usnewsmap.com" + path, { method: "POST", headers: { authorization: "Bearer " + token }, body: JSON.stringify(body) }), E, {});
  const body = JSON.stringify({ page: "p", a: "ᏒᎯ", b: "ᎡᎯ", a_is: "reference", b_is: "loc", lookalikes: [{ a: 0, a_label: "sv", b: 0, b_label: "e" }] });
  db.prepare("INSERT INTO tasks (id, lang, set_name, body) VALUES ('chr/s/1', 'chr', 's', ?), ('chr/s/2', 'chr', 's', ?)").run(body, body);
  const t1 = await signSession(env.SESSION_KEY, "c1"), t2 = await signSession(env.SESSION_KEY, "c2"), t3 = await signSession(env.SESSION_KEY, "c3");
  for (const [tok, choice] of [[t1, "a"], [t2, "a"], [t3, "b"]]) assert.equal((await post("/api/answers", tok, { task: "chr/s/1", choice })).status, 200);
  await post("/api/answers", t1, { task: "chr/s/2", choice: "b" });
  const row = (id) => ({ ...db.prepare("SELECT n, settled, outcome, confusable_display, held FROM tasks WHERE id = ?").get(id) });
  assert.deepEqual(row("chr/s/1"), { n: 3, settled: 1, outcome: "a", confusable_display: 0, held: 0 });

  // the two operations need the admin token and a well-formed body
  assert.equal((await post("/admin/confusable", "wrong", { action: "mark", tasks: ["chr/s/1"] })).status, 401);
  for (const bad of [{}, { action: "mark" }, { action: "mark", tasks: [] }, { action: "delete", tasks: ["x"] }, { action: "mark", tasks: [1] }]) {
    assert.equal((await adminCall(DB, "/admin/confusable", bad)).status, 400, JSON.stringify(bad));
  }

  const marked = await (await adminCall(DB, "/admin/confusable", { action: "mark", tasks: ["chr/s/1", "chr/s/1", "chr/s/missing"] })).json();
  assert.deepEqual(marked, { action: "mark", requested: 2, changed: ["chr/s/1"], unchanged: [], missing: ["chr/s/missing"] });
  // set aside, not deleted; unsettled; out of the queue; the other task untouched
  assert.deepEqual(row("chr/s/1"), { n: 0, settled: 0, outcome: null, confusable_display: 1, held: 1 });
  assert.equal(db.prepare("SELECT COUNT(*) AS n FROM answers WHERE task_id = 'chr/s/1'").get().n, 0);
  assert.deepEqual(db.prepare("SELECT contributor, choice FROM answers_excluded WHERE task_id = 'chr/s/1' ORDER BY contributor").all().map((r) => ({ ...r })),
    [{ contributor: "c1", choice: "a" }, { contributor: "c2", choice: "a" }, { contributor: "c3", choice: "b" }]);
  assert.deepEqual(row("chr/s/2"), { n: 1, settled: 0, outcome: null, confusable_display: 0, held: 0 });
  // marking again changes nothing (it would otherwise set aside answers given after the fix)
  assert.deepEqual((await (await adminCall(DB, "/admin/confusable", { action: "mark", tasks: ["chr/s/1"] })).json()).unchanged, ["chr/s/1"]);

  // held: not served, not answerable
  const fresh = await signSession(env.SESSION_KEY, "c9");
  assert.equal((await (await get("/api/next?lang=chr", fresh)).json()).task.id, "chr/s/2");
  assert.equal((await post("/api/answers", fresh, { task: "chr/s/1", choice: "a" })).status, 409);
  // the export marks it, counts nothing in `counts`, and keeps the set-aside answers apart
  const exported = async () => (await (await get("/api/export?lang=chr")).text()).trim().split("\n").map((l) => JSON.parse(l));
  const [x1, x2] = await exported();
  assert.deepEqual(x1.counts, { a: 0, b: 0, neither: 0, unsure: 0, notext: 0 });
  assert.deepEqual([x1.confusable_display, x1.held, x1.excluded_counts, x1.settled, x1.outcome], [true, true, { a: 2, b: 1, neither: 0, unsure: 0, notext: 0 }, false, null]);
  assert.deepEqual(x1.lookalikes, [{ a: 0, a_label: "sv", b: 0, b_label: "e" }]);
  assert.equal("confusable_display" in x2, false);
  const stats = await (await get("/api/stats?lang=chr")).json();
  assert.deepEqual([stats.answers, stats.people], [1, 1], "the set-aside answers are in no figure");

  // reopened: served again, including to the people who answered before, whose old answers stay excluded
  assert.deepEqual((await (await adminCall(DB, "/admin/confusable", { action: "reopen", tasks: ["chr/s/1", "chr/s/2"] })).json()).changed, ["chr/s/1"]);
  assert.equal(row("chr/s/1").held, 0);
  const next = await (await get("/api/next?lang=chr&skip=chr/s/2", t1)).json();
  assert.equal(next.task.id, "chr/s/1", "a person who answered before is asked again");
  // a page from before the fix (no display revision) can't answer a marked word; a current one can
  const stale = await post("/api/answers", t1, { task: "chr/s/1", choice: "a" });
  assert.equal(stale.status, 426);
  assert.equal((await post("/api/answers", t1, { task: "chr/s/1", choice: "a", display: DISPLAY_REVISION - 1 })).status, 426);
  assert.equal(db.prepare("SELECT COUNT(*) AS n FROM answers WHERE task_id = 'chr/s/1'").get().n, 0, "a refused answer is not stored");
  for (const [tok, choice] of [[t1, "b"], [t2, "b"], [t3, "b"]]) assert.equal((await post("/api/answers", tok, { task: "chr/s/1", choice, display: DISPLAY_REVISION })).status, 200);
  // tasks never marked take answers from any page, as before
  assert.equal((await post("/api/answers", t2, { task: "chr/s/2", choice: "a" })).status, 200);
  assert.deepEqual(row("chr/s/1"), { n: 3, settled: 1, outcome: "b", confusable_display: 1, held: 0 }, "only the new answers settle it");
  assert.equal(db.prepare("SELECT COUNT(*) AS n FROM answers_excluded WHERE task_id = 'chr/s/1'").get().n, 3, "the old answers stay set aside");
  const [y1] = await exported();
  assert.deepEqual([y1.counts.b, y1.counts.a, y1.excluded_counts.a, y1.outcome], [3, 0, 2, "b"]);
  // a mark request that read the task before it was marked (a racing one) runs its statements now: they change nothing
  const { MARK_CONFUSABLE_SQL } = await import("./worker.mjs");
  const res = await DB.batch(MARK_CONFUSABLE_SQL.map((sql) => DB.prepare(sql).bind("chr/s/1")));
  assert.equal(res[res.length - 1].meta.changes, 0);
  assert.equal(db.prepare("SELECT COUNT(*) AS n FROM answers WHERE task_id = 'chr/s/1'").get().n, 3, "the new answers are still active");
  assert.equal(db.prepare("SELECT COUNT(*) AS n FROM answers_excluded WHERE task_id = 'chr/s/1'").get().n, 3);
  assert.deepEqual(row("chr/s/1"), { n: 3, settled: 1, outcome: "b", confusable_display: 1, held: 0 });
  // reopening again, or marking again, does not disturb it
  assert.deepEqual((await (await adminCall(DB, "/admin/confusable", { action: "reopen", tasks: ["chr/s/1"] })).json()).unchanged, ["chr/s/1"]);
  assert.deepEqual((await (await adminCall(DB, "/admin/confusable", { action: "mark", tasks: ["chr/s/1"] })).json()).unchanged, ["chr/s/1"]);
  assert.equal(row("chr/s/1").outcome, "b");
  // an answer request from a page without the display revision that read the task before it was marked, reopened: refused
  const { ANSWER_SQL } = await import("./worker.mjs");
  db.prepare("DELETE FROM answers WHERE task_id = 'chr/s/1' AND contributor = 'c3'").run();
  assert.equal(db.prepare(ANSWER_SQL).run("chr/s/1", "c3", "a", 0, DISPLAY_REVISION - 1).changes, 0);
  assert.equal(db.prepare(ANSWER_SQL).run("chr/s/2", "c3", "a", 0, 0).changes, 1, "a task never marked takes it");
  assert.equal(db.prepare(ANSWER_SQL).run("chr/s/1", "c3", "b", 0, DISPLAY_REVISION).changes, 1);
  // loading a seed file again doesn't bring a set-aside seed answer back, and a new one still counts
  const seedPost = async (rows) => (await adminCall(DB, "/admin/answers", null, rows.map((r) => JSON.stringify(r)).join("\n"))).json();
  db.prepare("INSERT INTO answers_excluded (task_id, contributor, choice, at) VALUES ('chr/s/1', 'seed:old', 'a', 111)").run();
  const seeded = await seedPost([{ task: "chr/s/1", contributor: "seed:old", choice: "a", at: 111 }, { task: "chr/s/1", contributor: "seed:new", choice: "b", at: 222 }]);
  assert.deepEqual([seeded.answers, seeded.skipped], [1, 1]);
  assert.equal(db.prepare("SELECT COUNT(*) AS n FROM answers WHERE contributor = 'seed:old'").get().n, 0);
  assert.equal(db.prepare("SELECT COUNT(*) AS n FROM answers WHERE contributor = 'seed:new'").get().n, 1);
});

test("the page and the Worker agree on the display revision", () => {
  const app = readFileSync(new URL("./public/app.js", import.meta.url), "utf8");
  assert.equal(Number(app.match(/const DISPLAY = (\d+);/)[1]), DISPLAY_REVISION);
  assert.match(app, /JSON\.stringify\(\{ task: task\.id, choice, display: DISPLAY \}\)/);
});

test("marking is atomic: a copy that misses a row fails the batch and leaves the answers where they were", async () => {
  const { db, DB } = await sqliteD1();
  db.prepare("INSERT INTO tasks (id, lang, set_name, body) VALUES ('chr/s/1', 'chr', 's', '{}')").run();
  db.prepare("INSERT INTO answers (task_id, contributor, choice, at) VALUES ('chr/s/1', 'c1', 'a', 0), ('chr/s/1', 'c2', 'b', 0)").run();
  // a trigger that drops one copied row stands in for a lost write
  db.exec("CREATE TRIGGER lose AFTER INSERT ON answers_excluded WHEN NEW.contributor = 'c2' BEGIN DELETE FROM answers_excluded WHERE contributor = 'c2'; END");
  assert.equal((await adminCall(DB, "/admin/confusable", { action: "mark", tasks: ["chr/s/1"] })).status, 500);
  assert.equal(db.prepare("SELECT COUNT(*) AS n FROM answers WHERE task_id = 'chr/s/1'").get().n, 2);
  assert.equal(db.prepare("SELECT COUNT(*) AS n FROM answers_excluded").get().n, 0);
  assert.equal(db.prepare("SELECT confusable_display FROM tasks WHERE id = 'chr/s/1'").get().confusable_display, 0);
});

test("migration 0002 only adds, so the Worker from before it still runs on the migrated database", async () => {
  const { DatabaseSync } = await import("node:sqlite");
  const { readFileSync } = await import("node:fs");
  const { SETTLE_SQL } = await import("./worker.mjs");
  const db = new DatabaseSync(":memory:");
  // the schema as it was before this change
  db.exec(`CREATE TABLE tasks (id TEXT PRIMARY KEY, lang TEXT NOT NULL, set_name TEXT NOT NULL, body TEXT NOT NULL, n INTEGER NOT NULL DEFAULT 0, outcome TEXT, settled INTEGER NOT NULL DEFAULT 0);
    CREATE TABLE answers (task_id TEXT NOT NULL, contributor TEXT NOT NULL, choice TEXT NOT NULL CHECK (choice IN ('a', 'b', 'neither', 'unsure', 'notext')), at INTEGER NOT NULL, PRIMARY KEY (task_id, contributor));`);
  db.prepare("INSERT INTO tasks (id, lang, set_name, body) VALUES ('t', 'chr', 's', '{}')").run();
  db.prepare("INSERT INTO answers VALUES ('t', 'c1', 'a', 0)").run();
  db.exec(readFileSync(new URL("./migrations/0002-confusable-display.sql", import.meta.url), "utf8"));
  assert.deepEqual({ ...db.prepare("SELECT n, settled, confusable_display, held FROM tasks").get() }, { n: 0, settled: 0, confusable_display: 0, held: 0 });
  assert.equal(db.prepare("SELECT COUNT(*) AS n FROM answers").get().n, 1);
  // the statements the old Worker ran
  db.prepare("INSERT INTO answers (task_id, contributor, choice, at) VALUES ('t', 'c2', 'a', 0) ON CONFLICT (task_id, contributor) DO UPDATE SET choice = excluded.choice").run();
  db.prepare(SETTLE_SQL).run("t");
  assert.equal(db.prepare("SELECT n FROM tasks").get().n, 2);
  db.prepare("INSERT INTO tasks (id, lang, set_name, body) VALUES ('u', 'chr', 's', '{}') ON CONFLICT (id) DO UPDATE SET body = excluded.body").run();
  // and the schema.sql a new database gets has the same tables and columns as the migrated one
  const fresh = new DatabaseSync(":memory:");
  fresh.exec(readFileSync(new URL("./schema.sql", import.meta.url), "utf8"));
  const shape = (d) => ["tasks", "answers_excluded"].map((t) => d.prepare(`SELECT name, type, "notnull", dflt_value FROM pragma_table_info('${t}') ORDER BY name`).all().map((r) => ({ ...r })));
  assert.deepEqual(shape(db), shape(fresh));
});
