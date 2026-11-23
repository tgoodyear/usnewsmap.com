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

test("/admin/tasks replaces a task's body and leaves its answers, counts, outcome, settled, confusable_display, held and set-aside answers alone", async () => {
  const { db, DB } = await sqliteD1();
  const E = { ...env, DB };
  const old = { page: "p", a: "ᏒᎯ", b: "ᎡᎯ", a_is: "reference", b_is: "loc", lookalikes: [{ a: 0, a_label: "sv", b: 0, b_label: "e" }] };
  db.prepare("INSERT INTO tasks (id, lang, set_name, body) VALUES ('chr/s/1', 'chr', 's', ?), ('chr/s/2', 'chr', 's', ?)").run(JSON.stringify(old), JSON.stringify(old));
  const post = async (path, token, body) => worker.fetch(new Request("https://check.usnewsmap.com" + path, { method: "POST", headers: { authorization: "Bearer " + token }, body: JSON.stringify(body) }), E, {});
  for (const c of ["c1", "c2", "c3"]) assert.equal((await post("/api/answers", await signSession(env.SESSION_KEY, c), { task: "chr/s/1", choice: c === "c3" ? "b" : "a" })).status, 200);
  await post("/api/answers", await signSession(env.SESSION_KEY, "c1"), { task: "chr/s/2", choice: "a" });
  // task 2 is marked, so it has set-aside answers and is held
  assert.equal((await adminCall(DB, "/admin/confusable", { action: "mark", tasks: ["chr/s/2"] })).status, 200);
  db.prepare("UPDATE tasks SET held = 1 WHERE id = 'chr/s/2'").run();
  const state = () => ({
    tasks: db.prepare("SELECT id, lang, set_name, n, outcome, settled, confusable_display, held FROM tasks ORDER BY id").all().map((r) => ({ ...r })),
    answers: db.prepare("SELECT * FROM answers ORDER BY task_id, contributor").all().map((r) => ({ ...r })),
    excluded: db.prepare("SELECT * FROM answers_excluded ORDER BY task_id, contributor").all().map((r) => ({ ...r })),
  });
  const before = state();
  assert.deepEqual([before.tasks[0].n, before.tasks[0].settled, before.tasks[0].outcome], [3, 1, "a"]);
  assert.deepEqual([before.tasks[1].confusable_display, before.tasks[1].held, before.excluded.length], [1, 1, 1]);
  // the new bodies differ in lookalikes and mark_only only; the line carries id, lang and set as load.sh sends them
  const fresh = { ...old, mark_only: [{ a: 1, a_marks: "\u05b7", b: 1, b_marks: "", base: "\u05d0", differ: "\u05b7" }] };
  delete fresh.lookalikes;
  const lines = ["chr/s/1", "chr/s/2"].map((id) => JSON.stringify({ id, lang: "chr", set: "s", ...fresh })).join("\n");
  const r = await adminCall(DB, "/admin/tasks", null, lines);
  assert.deepEqual(await r.json(), { ok: true, tasks: 2 });
  for (const id of ["chr/s/1", "chr/s/2"]) assert.deepEqual(JSON.parse(db.prepare("SELECT body FROM tasks WHERE id = ?").get(id).body), fresh, "the body is replaced");
  assert.deepEqual(state(), before, "everything but the body is as it was");
});

test("/admin/tasks with if_body replaces a body only while it is still the one the sender read", async () => {
  const { db, DB } = await sqliteD1();
  const text = JSON.stringify({ page: "p", a: "x", b: "y" });
  db.prepare("INSERT INTO tasks (id, lang, set_name, body) VALUES ('yi/s/1', 'yi', 's', ?), ('yi/s/2', 'yi', 's', ?)").run(text, text);
  const line = (id, body, if_body, extra = {}) => JSON.stringify({ id, lang: "yi", set: "s", ...body, ...(if_body === undefined ? {} : { if_body }), ...extra });
  // task 2 was edited by another loader after the sender read it
  db.prepare("UPDATE tasks SET body = ? WHERE id = 'yi/s/2'").run(JSON.stringify({ page: "p", a: "x", b: "edited" }));
  const next = { page: "p", a: "x", b: "y", mark_only: [] };
  const r = await adminCall(DB, "/admin/tasks", null, [line("yi/s/1", next, text), line("yi/s/2", next, text), line("yi/s/missing", next, text)].join("\n"));
  assert.deepEqual(await r.json(), { ok: true, tasks: 1, stale: ["yi/s/2", "yi/s/missing"] });
  assert.deepEqual(JSON.parse(db.prepare("SELECT body FROM tasks WHERE id = 'yi/s/1'").get().body), next);
  assert.equal(JSON.parse(db.prepare("SELECT body FROM tasks WHERE id = 'yi/s/2'").get().body).b, "edited", "the other loader's edit is kept");
  assert.equal(db.prepare("SELECT COUNT(*) AS n FROM tasks").get().n, 2, "a conditional line never inserts");
  // a wrong lang or set is stale too, and a line without if_body behaves as before
  const r2 = await adminCall(DB, "/admin/tasks", null, line("yi/s/1", next, JSON.stringify(next), { lang: "sr" }));
  assert.deepEqual(await r2.json(), { ok: true, tasks: 0, stale: ["yi/s/1"] });
  assert.deepEqual(await (await adminCall(DB, "/admin/tasks", null, line("yi/s/3", next))).json(), { ok: true, tasks: 1 });
  assert.equal((await adminCall(DB, "/admin/tasks", null, line("yi/s/1", next, 5))).status, 400);
  // more than one batch of 50: the stale ids are still found
  const many = Array.from({ length: 120 }, (_, i) => line(`yi/s/m${i}`, next, "no such body")).join("\n");
  assert.equal((await (await adminCall(DB, "/admin/tasks", null, many)).json()).stale.length, 120);
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
  const answer = (task, who, choice) => { db.prepare(ANSWER_SQL).run(task, who, choice, 0, 0, "unknown", "unknown"); db.prepare(SETTLE_SQL).run(task); };
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
  const body = JSON.stringify({ page: "p", a: "ᏒᎯ", b: "ᎡᎯ", a_is: "reference", b_is: "loc", lookalikes: [{ a: 0, a_label: "sv", b: 0, b_label: "e" }], mark_only: [{ a: 1, a_marks: "\u05b7", b: 1, b_marks: "", base: "\u05d0" }] });
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
  assert.deepEqual(marked, { action: "mark", reason: "confusable_display", requested: 2, changed: ["chr/s/1"], unchanged: [], missing: ["chr/s/missing"] });
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
  assert.deepEqual(x1.mark_only, [{ a: 1, a_marks: "\u05b7", b: 1, b_marks: "", base: "\u05d0" }], "mark_only is exported as stored");
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
  const { markStatements } = await import("./worker.mjs");
  const res = await DB.batch(markStatements("chr/s/1", "confusable_display", 1).map(([sql, args]) => DB.prepare(sql).bind(...args)));
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
  assert.equal(db.prepare(ANSWER_SQL).run("chr/s/1", "c3", "a", 0, DISPLAY_REVISION - 1, "unknown", "unknown").changes, 0);
  assert.equal(db.prepare(ANSWER_SQL).run("chr/s/2", "c3", "a", 0, 0, "unknown", "unknown").changes, 1, "a task never marked takes it");
  assert.equal(db.prepare(ANSWER_SQL).run("chr/s/1", "c3", "b", 0, DISPLAY_REVISION, "unknown", "unknown").changes, 1);
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
  assert.match(app, /JSON\.stringify\(\{ task: task\.id, choice, display: DISPLAY, font: fontState\(task\), names_shown: namesState\(task\) \}\)/);
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
  // (without the font, names_shown and mark columns, which migrations 0003, 0004 and 0005 add)
  const shape = (d) => ["tasks", "answers_excluded"].map((t) => d.prepare(`SELECT name, type, "notnull", dflt_value FROM pragma_table_info('${t}') WHERE name NOT IN ('font', 'names_shown', 'mark_reason', 'outcome_withdrawn', 'withdrawn_reason', 'withdrawn_at', 'hold_reason') ORDER BY name`).all().map((r) => ({ ...r })));
  assert.deepEqual(shape(db), shape(fresh));
});

// ---------- the font each answer was given with (#1939) ----------

test("an answer stores the font state the page reported, and an old page's answer is unknown", async () => {
  const { db, DB } = await sqliteD1();
  const E = { ...env, DB };
  db.prepare("INSERT INTO tasks (id, lang, set_name, body) VALUES ('chr/s/1', 'chr', 's', '{\"a\":\"x\",\"b\":\"y\"}'), ('chr/s/2', 'chr', 's', '{}')").run();
  const post = async (name, body) => worker.fetch(new Request("https://check.usnewsmap.com/api/answers", { method: "POST", headers: { authorization: "Bearer " + (await signSession(env.SESSION_KEY, name)) }, body: JSON.stringify(body) }), E, {});
  const fontOfAnswer = (c) => db.prepare("SELECT font FROM answers WHERE task_id = 'chr/s/1' AND contributor = ?").get(c)?.font;
  for (const [c, font] of [["c1", "loaded"], ["c2", "not_loaded"], ["c3", "none"], ["c4", "unknown"]]) assert.equal((await post(c, { task: "chr/s/1", choice: "a", font })).status, 200);
  // a cached page from before the value existed sends none; so does a value the Worker doesn't know
  assert.equal((await post("c5", { task: "chr/s/1", choice: "a" })).status, 200);
  assert.equal((await post("c6", { task: "chr/s/1", choice: "a", font: "Arial, Times New Roman" })).status, 200);
  assert.equal((await post("c7", { task: "chr/s/1", choice: "a", font: ["loaded"] })).status, 200);
  assert.deepEqual(["c1", "c2", "c3", "c4", "c5", "c6", "c7"].map(fontOfAnswer), ["loaded", "not_loaded", "none", "unknown", "unknown", "unknown", "unknown"]);
  // changing an answer takes the new answer's state
  await post("c1", { task: "chr/s/1", choice: "b", font: "none" });
  assert.deepEqual([db.prepare("SELECT choice, font FROM answers WHERE contributor = 'c1'").get()].map((r) => ({ ...r })), [{ choice: "b", font: "none" }]);
  // seed answers are unknown
  const seeded = await (await adminCall(DB, "/admin/answers", null, JSON.stringify({ task: "chr/s/2", contributor: "seed:x", choice: "a", at: 5 }))).json();
  assert.equal(seeded.answers, 1);
  assert.equal(db.prepare("SELECT font FROM answers WHERE contributor = 'seed:x'").get().font, "unknown");
  // the database itself refuses a value outside the list
  assert.throws(() => db.prepare("UPDATE answers SET font = 'Arial' WHERE contributor = 'c1'").run());
  // the export has the counts by state per task, adding up to the answers; nothing about a contributor
  const lines = (await (await worker.fetch(new Request("https://check.usnewsmap.com/api/export?lang=chr"), E, {})).text()).trim().split("\n").map((l) => JSON.parse(l));
  assert.deepEqual(lines[0].font_counts, { loaded: 0, not_loaded: 1, none: 2, unknown: 4 });
  assert.equal(Object.values(lines[0].font_counts).reduce((x, y) => x + y, 0), Object.values(lines[0].counts).reduce((x, y) => x + y, 0));
  assert.deepEqual(lines[1].font_counts, { loaded: 0, not_loaded: 0, none: 0, unknown: 1 });
  assert.equal("excluded_font_counts" in lines[0], false);
  // an answer set aside later keeps its state, and the export counts it apart
  await adminCall(DB, "/admin/confusable", { action: "mark", tasks: ["chr/s/1"] });
  assert.deepEqual(db.prepare("SELECT contributor, font FROM answers_excluded ORDER BY contributor").all().map((r) => ({ ...r })).slice(0, 3),
    [{ contributor: "c1", font: "none" }, { contributor: "c2", font: "not_loaded" }, { contributor: "c3", font: "none" }]);
  const [marked] = (await (await worker.fetch(new Request("https://check.usnewsmap.com/api/export?lang=chr"), E, {})).text()).trim().split("\n").map((l) => JSON.parse(l));
  assert.deepEqual(marked.font_counts, { loaded: 0, not_loaded: 0, none: 0, unknown: 0 });
  assert.deepEqual(marked.excluded_font_counts, { loaded: 0, not_loaded: 1, none: 2, unknown: 4 });
  // and the keys the older readers use are unchanged
  assert.deepEqual(Object.keys(marked.counts), ["a", "b", "neither", "unsure", "notext"]);
  assert.deepEqual(Object.keys(marked.excluded_counts), ["a", "b", "neither", "unsure", "notext"]);
});

test("migration 0003 only adds, so the Worker from before it still runs on the migrated database", async () => {
  const { DatabaseSync } = await import("node:sqlite");
  const db = new DatabaseSync(":memory:");
  // the schema and the statements as they were after migration 0002
  db.exec(`CREATE TABLE tasks (id TEXT PRIMARY KEY, lang TEXT NOT NULL, set_name TEXT NOT NULL, body TEXT NOT NULL, n INTEGER NOT NULL DEFAULT 0, outcome TEXT, settled INTEGER NOT NULL DEFAULT 0, confusable_display INTEGER NOT NULL DEFAULT 0, held INTEGER NOT NULL DEFAULT 0);
    CREATE TABLE answers (task_id TEXT NOT NULL, contributor TEXT NOT NULL, choice TEXT NOT NULL CHECK (choice IN ('a', 'b', 'neither', 'unsure', 'notext')), at INTEGER NOT NULL, PRIMARY KEY (task_id, contributor));
    CREATE TABLE answers_excluded (task_id TEXT NOT NULL, contributor TEXT NOT NULL, choice TEXT NOT NULL CHECK (choice IN ('a', 'b', 'neither', 'unsure', 'notext')), at INTEGER NOT NULL, PRIMARY KEY (task_id, contributor));`);
  db.prepare("INSERT INTO tasks (id, lang, set_name, body) VALUES ('t', 'chr', 's', '{}')").run();
  db.prepare("INSERT INTO answers VALUES ('t', 'c1', 'a', 0)").run();
  db.prepare("INSERT INTO answers_excluded VALUES ('t', 'c0', 'b', 0)").run();
  db.exec(readFileSync(new URL("./migrations/0003-answer-font.sql", import.meta.url), "utf8"));
  assert.deepEqual({ ...db.prepare("SELECT contributor, choice, font FROM answers").get() }, { contributor: "c1", choice: "a", font: "unknown" });
  assert.equal(db.prepare("SELECT font FROM answers_excluded").get().font, "unknown", "existing answers are unknown");
  // the statements the Worker of the 0002 revision ran
  db.prepare(`INSERT INTO answers (task_id, contributor, choice, at)
    SELECT ?1, ?2, ?3, ?4 WHERE NOT EXISTS (SELECT 1 FROM tasks WHERE id = ?1 AND (held = 1 OR (confusable_display = 1 AND ?5 < 1)))
    ON CONFLICT (task_id, contributor) DO UPDATE SET choice = excluded.choice, at = excluded.at`).run("t", "c2", "b", 1, 1);
  db.prepare("INSERT INTO answers_excluded (task_id, contributor, choice, at) SELECT task_id, contributor, choice, at FROM answers WHERE task_id = 't'").run();
  assert.deepEqual(db.prepare("SELECT font FROM answers ORDER BY contributor").all().map((r) => r.font), ["unknown", "unknown"]);
  // the schema.sql a new database gets has the same columns as the migrated one
  const fresh = new DatabaseSync(":memory:");
  fresh.exec(readFileSync(new URL("./schema.sql", import.meta.url), "utf8"));
  const shape = (d) => ["tasks", "answers", "answers_excluded"].map((t) => d.prepare(`SELECT name, type, "notnull", dflt_value FROM pragma_table_info('${t}') WHERE name NOT IN ('names_shown', 'mark_reason', 'outcome_withdrawn', 'withdrawn_reason', 'withdrawn_at', 'hold_reason') ORDER BY name`).all().map((r) => ({ ...r })));
  assert.deepEqual(shape(db), shape(fresh));
  // and the new Worker's statements run on the migrated database once migration 0004 has added its column
  db.exec(readFileSync(new URL("./migrations/0004-answer-names.sql", import.meta.url), "utf8"));
  const { ANSWER_SQL } = await import("./worker.mjs");
  assert.equal(db.prepare(ANSWER_SQL).run("t", "c3", "a", 2, 1, "loaded", "unknown").changes, 1);
  assert.equal(db.prepare("SELECT font FROM answers WHERE contributor = 'c3'").get().font, "loaded");
});

// ---------- whether letter names were showing when each answer was given ----------

test("an answer stores whether letter names were showing, and an old page's answer is unknown", async () => {
  const { db, DB } = await sqliteD1();
  const E = { ...env, DB };
  db.prepare("INSERT INTO tasks (id, lang, set_name, body) VALUES ('chr/s/1', 'chr', 's', '{\"a\":\"x\",\"b\":\"y\"}'), ('chr/s/2', 'chr', 's', '{}')").run();
  const post = async (name, body) => worker.fetch(new Request("https://check.usnewsmap.com/api/answers", { method: "POST", headers: { authorization: "Bearer " + (await signSession(env.SESSION_KEY, name)) }, body: JSON.stringify(body) }), E, {});
  const namesOfAnswer = (c) => db.prepare("SELECT names_shown FROM answers WHERE task_id = 'chr/s/1' AND contributor = ?").get(c)?.names_shown;
  for (const [c, names_shown] of [["c1", "shown"], ["c2", "not_shown"], ["c3", "unknown"]]) assert.equal((await post(c, { task: "chr/s/1", choice: "a", font: "loaded", names_shown })).status, 200);
  // a cached page from before the value existed sends none; so does a value the Worker doesn't know
  assert.equal((await post("c4", { task: "chr/s/1", choice: "a" })).status, 200);
  assert.equal((await post("c5", { task: "chr/s/1", choice: "a", names_shown: "yes" })).status, 200);
  assert.equal((await post("c6", { task: "chr/s/1", choice: "a", names_shown: ["shown"] })).status, 200);
  assert.equal((await post("c7", { task: "chr/s/1", choice: "a", names_shown: true })).status, 200);
  assert.deepEqual(["c1", "c2", "c3", "c4", "c5", "c6", "c7"].map(namesOfAnswer), ["shown", "not_shown", "unknown", "unknown", "unknown", "unknown", "unknown"]);
  // changing an answer takes the new answer's value, and the font value is stored independently
  await post("c1", { task: "chr/s/1", choice: "b", names_shown: "not_shown" });
  assert.deepEqual([db.prepare("SELECT choice, font, names_shown FROM answers WHERE contributor = 'c1'").get()].map((r) => ({ ...r })), [{ choice: "b", font: "unknown", names_shown: "not_shown" }]);
  // seed answers are unknown, and the database itself refuses a value outside the list
  const seeded = await (await adminCall(DB, "/admin/answers", null, JSON.stringify({ task: "chr/s/2", contributor: "seed:x", choice: "a", at: 5 }))).json();
  assert.equal(seeded.answers, 1);
  assert.equal(db.prepare("SELECT names_shown FROM answers WHERE contributor = 'seed:x'").get().names_shown, "unknown");
  assert.throws(() => db.prepare("UPDATE answers SET names_shown = 'Arial' WHERE contributor = 'c1'").run());
  // the export has the counts per task, adding up to the answers; the other counts are as before
  await post("c8", { task: "chr/s/1", choice: "a", names_shown: "shown" });
  const read = async () => (await (await worker.fetch(new Request("https://check.usnewsmap.com/api/export?lang=chr"), E, {})).text()).trim().split("\n").map((l) => JSON.parse(l));
  const lines = await read();
  assert.deepEqual(lines[0].names_counts, { shown: 1, not_shown: 2, unknown: 5 });
  assert.equal(Object.values(lines[0].names_counts).reduce((x, y) => x + y, 0), Object.values(lines[0].counts).reduce((x, y) => x + y, 0));
  assert.deepEqual(lines[1].names_counts, { shown: 0, not_shown: 0, unknown: 1 });
  assert.equal("excluded_names_counts" in lines[0], false);
  assert.deepEqual(Object.keys(lines[0].counts), ["a", "b", "neither", "unsure", "notext"]);
  assert.deepEqual(lines[0].font_counts, { loaded: 2, not_loaded: 0, none: 0, unknown: 6 });
  // an answer set aside later keeps its value, and the export counts it apart
  await adminCall(DB, "/admin/confusable", { action: "mark", tasks: ["chr/s/1"] });
  assert.deepEqual(db.prepare("SELECT contributor, names_shown FROM answers_excluded ORDER BY contributor").all().map((r) => ({ ...r })).slice(0, 3),
    [{ contributor: "c1", names_shown: "not_shown" }, { contributor: "c2", names_shown: "not_shown" }, { contributor: "c3", names_shown: "unknown" }]);
  const [marked] = await read();
  assert.deepEqual(marked.names_counts, { shown: 0, not_shown: 0, unknown: 0 });
  assert.deepEqual(marked.excluded_names_counts, { shown: 1, not_shown: 2, unknown: 5 });
  assert.deepEqual(Object.keys(marked.excluded_counts), ["a", "b", "neither", "unsure", "notext"]);
});

test("migration 0004 only adds, so the Worker from before it still runs on the migrated database", async () => {
  const { DatabaseSync } = await import("node:sqlite");
  const db = new DatabaseSync(":memory:");
  // the schema and the statements as they were after migration 0003
  db.exec(`CREATE TABLE tasks (id TEXT PRIMARY KEY, lang TEXT NOT NULL, set_name TEXT NOT NULL, body TEXT NOT NULL, n INTEGER NOT NULL DEFAULT 0, outcome TEXT, settled INTEGER NOT NULL DEFAULT 0, confusable_display INTEGER NOT NULL DEFAULT 0, held INTEGER NOT NULL DEFAULT 0);
    CREATE TABLE answers (task_id TEXT NOT NULL, contributor TEXT NOT NULL, choice TEXT NOT NULL CHECK (choice IN ('a', 'b', 'neither', 'unsure', 'notext')), at INTEGER NOT NULL, font TEXT NOT NULL DEFAULT 'unknown' CHECK (font IN ('loaded', 'not_loaded', 'none', 'unknown')), PRIMARY KEY (task_id, contributor));
    CREATE TABLE answers_excluded (task_id TEXT NOT NULL, contributor TEXT NOT NULL, choice TEXT NOT NULL CHECK (choice IN ('a', 'b', 'neither', 'unsure', 'notext')), at INTEGER NOT NULL, font TEXT NOT NULL DEFAULT 'unknown' CHECK (font IN ('loaded', 'not_loaded', 'none', 'unknown')), PRIMARY KEY (task_id, contributor));`);
  db.prepare("INSERT INTO tasks (id, lang, set_name, body) VALUES ('t', 'chr', 's', '{}')").run();
  db.prepare("INSERT INTO answers VALUES ('t', 'c1', 'a', 0, 'loaded')").run();
  db.prepare("INSERT INTO answers_excluded VALUES ('t', 'c0', 'b', 0, 'none')").run();
  db.exec(readFileSync(new URL("./migrations/0004-answer-names.sql", import.meta.url), "utf8"));
  assert.deepEqual({ ...db.prepare("SELECT contributor, font, names_shown FROM answers").get() }, { contributor: "c1", font: "loaded", names_shown: "unknown" });
  assert.equal(db.prepare("SELECT names_shown FROM answers_excluded").get().names_shown, "unknown", "existing answers are unknown");
  // the statements the Worker of the 0003 revision ran
  db.prepare(`INSERT INTO answers (task_id, contributor, choice, at, font)
    SELECT ?1, ?2, ?3, ?4, ?6 WHERE NOT EXISTS (SELECT 1 FROM tasks WHERE id = ?1 AND (held = 1 OR (confusable_display = 1 AND ?5 < 1)))
    ON CONFLICT (task_id, contributor) DO UPDATE SET choice = excluded.choice, at = excluded.at, font = excluded.font`).run("t", "c2", "b", 1, 1, "none");
  db.prepare("INSERT INTO answers_excluded (task_id, contributor, choice, at, font) SELECT task_id, contributor, choice, at, font FROM answers WHERE task_id = 't'").run();
  assert.deepEqual(db.prepare("SELECT names_shown FROM answers ORDER BY contributor").all().map((r) => r.names_shown), ["unknown", "unknown"]);
  // the schema.sql a new database gets has the same columns as the migrated one
  const fresh = new DatabaseSync(":memory:");
  fresh.exec(readFileSync(new URL("./schema.sql", import.meta.url), "utf8"));
  const shape = (d) => ["tasks", "answers", "answers_excluded"].map((t) => d.prepare(`SELECT name, type, "notnull", dflt_value FROM pragma_table_info('${t}') WHERE name NOT IN ('mark_reason', 'outcome_withdrawn', 'withdrawn_reason', 'withdrawn_at', 'hold_reason') ORDER BY name`).all().map((r) => ({ ...r })));
  assert.deepEqual(shape(db), shape(fresh));
  // and the new Worker's statements run on the migrated database
  const { ANSWER_SQL } = await import("./worker.mjs");
  assert.equal(db.prepare(ANSWER_SQL).run("t", "c3", "a", 2, 1, "loaded", "shown").changes, 1);
  assert.equal(db.prepare("SELECT names_shown FROM answers WHERE contributor = 'c3'").get().names_shown, "shown");
});

test("the page sends a value the Worker stores, and a language with no letter names always says not_shown", async () => {
  const app = readFileSync(new URL("./public/app.js", import.meta.url), "utf8");
  const { NAMES_STATES } = await import("./worker.mjs");
  const sent = ["shown", "not_shown"];
  for (const v of sent) assert.ok(NAMES_STATES.includes(v), v);
  for (const v of sent) assert.match(app, new RegExp(`"${v}"`));
  assert.match(app, /if \(!namesOn\(\) \|\| !t\) return "not_shown";/);
  assert.match(readFileSync(new URL("./migrations/0004-answer-names.sql", import.meta.url), "utf8"), /IN \('shown', 'not_shown', 'unknown'\)/);
});

test("namesState looks at each reading on its own, so a leading combining mark in one cannot hide the other's names", () => {
  const app = readFileSync(new URL("./public/app.js", import.meta.url), "utf8");
  const start = app.indexOf("const segmenter"), end = app.indexOf("function examples(");
  const make = (cfg, on) => new Function(`const cfg = ${JSON.stringify(cfg)}, showNames = ${on};\n${app.slice(start, end)}\nconst namesOn = () => showNames && hasNames();\n${app.slice(app.indexOf("function namesState"), app.indexOf("// What the page reports with an answer about the bundled font"))}\nreturn namesState;`)();
  const labels = { labels: { "Ꭰ": "a" } };
  assert.equal(make(labels, true)({ a: "Ꭰ", b: "\u0301x" }), "shown", "b starts with a combining mark");
  assert.equal(make(labels, true)({ a: "\u0301x", b: "Ꭰ" }), "shown");
  assert.equal(make(labels, true)({ a: "x", b: "y" }), "not_shown", "no named letter");
  assert.equal(make(labels, false)({ a: "Ꭰ", b: "Ꭰ" }), "not_shown", "option off");
  assert.equal(make({}, true)({ a: "Ꭰ", b: "Ꭰ" }), "not_shown", "a language with no letter names");
});

// ---------- words cut from a merged line (#1978) ----------

test("marking a task merged_line sets its answers aside, withdraws a settled outcome, holds it and never reopens it", async () => {
  const { db, DB } = await sqliteD1();
  const E = { ...env, DB };
  const get = async (path, token) => worker.fetch(new Request("https://check.usnewsmap.com" + path, { headers: token ? { authorization: "Bearer " + token } : {} }), E, {});
  const post = async (path, token, body) => worker.fetch(new Request("https://check.usnewsmap.com" + path, { method: "POST", headers: { authorization: "Bearer " + token }, body: JSON.stringify(body) }), E, {});
  const body = JSON.stringify({ page: "p", a: "ᏒᎯ", b: "ᎡᎯ", a_is: "reference", b_is: "loc" });
  for (const id of ["chr/s/1", "chr/s/2", "chr/s/3", "chr/s/4", "chr/s/5"]) db.prepare("INSERT INTO tasks (id, lang, set_name, body) VALUES (?, 'chr', 's', ?)").run(id, body);
  const [t1, t2, t3] = await Promise.all(["c1", "c2", "c3"].map((c) => signSession(env.SESSION_KEY, c)));
  for (const [tok, choice] of [[t1, "a"], [t2, "a"], [t3, "b"]]) assert.equal((await post("/api/answers", tok, { task: "chr/s/1", choice })).status, 200); // settles: a
  await post("/api/answers", t1, { task: "chr/s/2", choice: "b" }); // one answer, unsettled
  for (const c of ["x1", "x2", "x3"]) await post("/api/answers", await signSession(env.SESSION_KEY, c), { task: "chr/s/3", choice: "neither" }); // settles: neither
  for (let i = 0; i < 7; i++) await post("/api/answers", await signSession(env.SESSION_KEY, "d" + i), { task: "chr/s/4", choice: ["a", "b", "neither"][i % 3] }); // disputed
  const row = (id) => ({ ...db.prepare("SELECT n, settled, outcome, confusable_display, held, mark_reason, outcome_withdrawn, withdrawn_reason, withdrawn_at FROM tasks WHERE id = ?").get(id) });
  assert.deepEqual([row("chr/s/1").settled, row("chr/s/1").outcome, row("chr/s/4").outcome], [1, "a", "disputed"]);

  // the reason is checked; without one the request means confusable_display, as before
  for (const reason of ["other", 1, null]) assert.equal((await adminCall(DB, "/admin/confusable", { action: "mark", tasks: ["chr/s/5"], reason })).status, 400, String(reason));
  const before = Date.now();
  const r = await (await adminCall(DB, "/admin/confusable", { action: "mark", reason: "merged_line", tasks: ["chr/s/1", "chr/s/2", "chr/s/3", "chr/s/4", "chr/s/missing"] })).json();
  assert.deepEqual([r.reason, r.changed, r.missing], ["merged_line", ["chr/s/1", "chr/s/2", "chr/s/3", "chr/s/4"], ["chr/s/missing"]]);

  // a settled task: its outcome is withdrawn and kept; it counts as nothing
  const one = row("chr/s/1");
  assert.deepEqual({ ...one, withdrawn_at: typeof one.withdrawn_at }, { n: 0, settled: 0, outcome: null, confusable_display: 0, held: 1, mark_reason: "merged_line", outcome_withdrawn: "a", withdrawn_reason: "merged_line", withdrawn_at: "number" });
  assert.ok(one.withdrawn_at >= before);
  assert.equal(row("chr/s/3").outcome_withdrawn, "neither");
  assert.equal(row("chr/s/4").outcome_withdrawn, "disputed");
  // an unsettled one withdraws nothing
  assert.deepEqual(row("chr/s/2"), { n: 0, settled: 0, outcome: null, confusable_display: 0, held: 1, mark_reason: "merged_line", outcome_withdrawn: null, withdrawn_reason: null, withdrawn_at: null });
  // nothing is deleted: the answers are in answers_excluded
  assert.equal(db.prepare("SELECT COUNT(*) AS n FROM answers WHERE task_id IN ('chr/s/1','chr/s/2','chr/s/3','chr/s/4')").get().n, 0);
  assert.deepEqual(db.prepare("SELECT choice FROM answers_excluded WHERE task_id = 'chr/s/1' ORDER BY contributor").all().map((x) => x.choice), ["a", "a", "b"]);
  assert.equal(db.prepare("SELECT COUNT(*) AS n FROM answers_excluded WHERE task_id = 'chr/s/4'").get().n, 7);
  // a task never marked is as it was
  assert.deepEqual(row("chr/s/5"), { n: 0, settled: 0, outcome: null, confusable_display: 0, held: 0, mark_reason: null, outcome_withdrawn: null, withdrawn_reason: null, withdrawn_at: null });

  // held: not served, not answerable, not reopened by "reopen", not marked again
  const fresh = await signSession(env.SESSION_KEY, "c9");
  assert.equal((await (await get("/api/next?lang=chr", fresh)).json()).task.id, "chr/s/5");
  assert.equal((await post("/api/answers", fresh, { task: "chr/s/1", choice: "a", display: DISPLAY_REVISION })).status, 409);
  const re = await (await adminCall(DB, "/admin/confusable", { action: "reopen", tasks: ["chr/s/1", "chr/s/2"] })).json();
  assert.deepEqual([re.changed, re.unchanged], [[], ["chr/s/1", "chr/s/2"]]);
  assert.equal(row("chr/s/1").held, 1);
  assert.deepEqual((await (await adminCall(DB, "/admin/confusable", { action: "mark", reason: "merged_line", tasks: ["chr/s/1"] })).json()).unchanged, ["chr/s/1"]);
  assert.equal(row("chr/s/1").withdrawn_at, one.withdrawn_at, "a second mark does not rewrite the record");
  assert.deepEqual((await (await adminCall(DB, "/admin/confusable", { action: "mark", reason: "confusable_display", tasks: ["chr/s/1"] })).json()).unchanged, ["chr/s/1"], "a marked task keeps its first reason");

  // the export: unsettled, the reason, the withdrawn outcome, the excluded counts; not a confusable_display word
  const rows = (await (await get("/api/export?lang=chr")).text()).trim().split("\n").map((l) => JSON.parse(l));
  const x1 = rows.find((x) => x.id === "chr/s/1");
  assert.deepEqual([x1.settled, x1.outcome, x1.held, x1.mark_reason, x1.outcome_withdrawn, x1.withdrawn_reason, typeof x1.withdrawn_at], [false, null, true, "merged_line", "a", "merged_line", "number"]);
  assert.deepEqual([x1.counts, x1.excluded_counts], [{ a: 0, b: 0, neither: 0, unsure: 0, notext: 0 }, { a: 2, b: 1, neither: 0, unsure: 0, notext: 0 }]);
  assert.equal("confusable_display" in x1, false);
  const x2 = rows.find((x) => x.id === "chr/s/2");
  assert.equal("outcome_withdrawn" in x2, false);
  const x5 = rows.find((x) => x.id === "chr/s/5");
  assert.deepEqual(["mark_reason" in x5, "held" in x5, "outcome_withdrawn" in x5, "excluded_counts" in x5], [false, false, false, false]);
  // settled totals and the answers figure leave the marked tasks out
  const stats = await (await get("/api/stats?lang=chr")).json();
  assert.deepEqual([stats.tasks, stats.settled, stats.answers, stats.people], [5, 0, 0, 0]);
  const langs = await (await get("/api/languages")).json().catch(() => null);
  if (langs) assert.equal(langs.find((l) => l.code === "chr")?.settled ?? 0, 0);
  // the dump (the nightly backup) carries the columns
  const dumped = (await (await worker.fetch(new Request("https://check.usnewsmap.com/admin/dump", { headers: { authorization: "Bearer secret-admin" } }), E, {})).text()).trim().split("\n").map((l) => JSON.parse(l)).find((l) => l.table === "tasks" && l.row.id === "chr/s/1");
  assert.deepEqual([dumped.row.mark_reason, dumped.row.outcome_withdrawn], ["merged_line", "a"]);
});

test("a confusable_display mark also keeps a settled outcome, and a task marked before reasons existed still reads as confusable_display", async () => {
  const { db, DB } = await sqliteD1();
  const E = { ...env, DB };
  db.prepare("INSERT INTO tasks (id, lang, set_name, body, n, settled, outcome) VALUES ('chr/s/1', 'chr', 's', '{\"a\":\"x\"}', 3, 1, 'b')").run();
  db.prepare("INSERT INTO answers (task_id, contributor, choice, at) VALUES ('chr/s/1', 'c1', 'b', 1), ('chr/s/1', 'c2', 'b', 2), ('chr/s/1', 'c3', 'b', 3)").run();
  // marked by the Worker before reasons existed: confusable_display = 1, held, no reason
  db.prepare("INSERT INTO tasks (id, lang, set_name, body, confusable_display, held) VALUES ('chr/s/old', 'chr', 's', '{\"a\":\"x\"}', 1, 1)").run();
  const done = await (await adminCall(DB, "/admin/confusable", { action: "mark", tasks: ["chr/s/1", "chr/s/old"] })).json();
  assert.deepEqual([done.reason, done.changed, done.unchanged], ["confusable_display", ["chr/s/1"], ["chr/s/old"]], "marking the old one again does nothing");
  assert.deepEqual({ ...db.prepare("SELECT mark_reason, confusable_display, outcome_withdrawn, withdrawn_reason FROM tasks WHERE id = 'chr/s/1'").get() }, { mark_reason: "confusable_display", confusable_display: 1, outcome_withdrawn: "b", withdrawn_reason: "confusable_display" });
  const rows = (await (await worker.fetch(new Request("https://check.usnewsmap.com/api/export?lang=chr"), E, {})).text()).trim().split("\n").map((l) => JSON.parse(l));
  const old = rows.find((x) => x.id === "chr/s/old");
  assert.deepEqual([old.mark_reason, old.confusable_display, old.held, old.settled], ["confusable_display", true, true, false]);
  assert.equal("outcome_withdrawn" in old, false);
  // reopen still works for a confusable_display word, with or without a stored reason
  assert.deepEqual((await (await adminCall(DB, "/admin/confusable", { action: "reopen", tasks: ["chr/s/1", "chr/s/old"] })).json()).changed, ["chr/s/1", "chr/s/old"]);
});

test("migration 0005 only adds, so the Worker from before it still runs on the migrated database", async () => {
  const { DatabaseSync } = await import("node:sqlite");
  const db = new DatabaseSync(":memory:");
  // the tasks table as it was after migration 0004; the answers tables are not touched by 0005
  db.exec(`CREATE TABLE tasks (id TEXT PRIMARY KEY, lang TEXT NOT NULL, set_name TEXT NOT NULL, body TEXT NOT NULL, n INTEGER NOT NULL DEFAULT 0, outcome TEXT, settled INTEGER NOT NULL DEFAULT 0, confusable_display INTEGER NOT NULL DEFAULT 0, held INTEGER NOT NULL DEFAULT 0);`);
  db.prepare("INSERT INTO tasks (id, lang, set_name, body, n, outcome, settled, confusable_display, held) VALUES ('t', 'chr', 's', '{}', 3, 'a', 1, 0, 0), ('m', 'chr', 's', '{}', 0, NULL, 0, 1, 1)").run();
  db.exec(readFileSync(new URL("./migrations/0005-mark-reason.sql", import.meta.url), "utf8"));
  assert.deepEqual(db.prepare("SELECT id, n, outcome, settled, confusable_display, held, mark_reason, outcome_withdrawn, withdrawn_reason, withdrawn_at FROM tasks ORDER BY id").all().map((r) => ({ ...r })), [
    { id: "m", n: 0, outcome: null, settled: 0, confusable_display: 1, held: 1, mark_reason: null, outcome_withdrawn: null, withdrawn_reason: null, withdrawn_at: null },
    { id: "t", n: 3, outcome: "a", settled: 1, confusable_display: 0, held: 0, mark_reason: null, outcome_withdrawn: null, withdrawn_reason: null, withdrawn_at: null },
  ], "every existing task keeps its values; the new columns are empty");
  // the statements the Worker of the 0004 revision ran on tasks still work
  db.prepare("INSERT INTO tasks (id, lang, set_name, body) VALUES ('u', 'chr', 's', '{}') ON CONFLICT (id) DO UPDATE SET body = excluded.body").run();
  db.prepare("UPDATE tasks SET confusable_display = 1, held = 1, n = 0, settled = 0, outcome = NULL WHERE id = 't' AND confusable_display = 0").run();
  db.prepare("UPDATE tasks SET held = 0 WHERE id = 'm' AND confusable_display = 1 AND held = 1").run();
  assert.equal(db.prepare("SELECT held FROM tasks WHERE id = 'm'").get().held, 0);
  // a reason outside the two is refused by the column
  assert.throws(() => db.prepare("UPDATE tasks SET mark_reason = 'other' WHERE id = 'u'").run());
  // the schema.sql a new database gets has the same tasks columns as the migrated one
  const fresh = new DatabaseSync(":memory:");
  fresh.exec(readFileSync(new URL("./schema.sql", import.meta.url), "utf8"));
  const shape = (d) => d.prepare(`SELECT name, type, "notnull", dflt_value FROM pragma_table_info('tasks') ORDER BY name`).all().map((r) => ({ ...r }));
  assert.deepEqual(shape(db), shape(fresh));
});

test("a confusable_display task can move to merged_line: held ones just change reason, reopened ones keep every answer and withdraw the current outcome", async () => {
  const { db, DB } = await sqliteD1();
  const E = { ...env, DB };
  const post = async (path, token, body) => worker.fetch(new Request("https://check.usnewsmap.com" + path, { method: "POST", headers: { authorization: "Bearer " + token }, body: JSON.stringify(body) }), E, {});
  const mark = async (reason, ...tasks) => (await adminCall(DB, "/admin/confusable", { action: "mark", reason, tasks })).json();
  const row = (id) => ({ ...db.prepare("SELECT n, settled, outcome, confusable_display, held, mark_reason, outcome_withdrawn, withdrawn_reason FROM tasks WHERE id = ?").get(id) });
  const ids = ["chr/s/held", "chr/s/again", "chr/s/clash", "chr/s/twice"];
  for (const id of ids) db.prepare("INSERT INTO tasks (id, lang, set_name, body) VALUES (?, 'chr', 's', '{}')").run(id);
  const tok = {};
  for (const c of ["c1", "c2", "c3"]) tok[c] = await signSession(env.SESSION_KEY, c);
  const answer = async (id, c, choice) => assert.equal((await post("/api/answers", tok[c], { task: id, choice, display: DISPLAY_REVISION })).status, 200);
  for (const id of ids) for (const [c, choice] of [["c1", "a"], ["c2", "a"], ["c3", "b"]]) await answer(id, c, choice); // each settles on a
  assert.deepEqual((await mark("confusable_display", ...ids)).changed, ids);
  assert.deepEqual(row("chr/s/held").outcome_withdrawn, "a");
  // held and never reopened: only the reason changes (the older withdrawn outcome stays)
  assert.deepEqual((await mark("merged_line", "chr/s/held")).changed, ["chr/s/held"]);
  assert.deepEqual(row("chr/s/held"), { n: 0, settled: 0, outcome: null, confusable_display: 0, held: 1, mark_reason: "merged_line", outcome_withdrawn: "a", withdrawn_reason: "confusable_display" });
  assert.deepEqual((await mark("merged_line", "chr/s/held")).unchanged, ["chr/s/held"], "a second request changes nothing");
  assert.deepEqual((await adminCall(DB, "/admin/confusable", { action: "reopen", tasks: ["chr/s/held"] }).then((r) => r.json())).changed, [], "and it can't be reopened now");
  assert.deepEqual((await mark("confusable_display", "chr/s/held")).unchanged, ["chr/s/held"], "merged_line does not go back");

  // reopened, answered by new people in a different way, settled on b: the move keeps the new answers and withdraws "b"
  await adminCall(DB, "/admin/confusable", { action: "reopen", tasks: ["chr/s/again"] });
  db.prepare("INSERT INTO answers (task_id, contributor, choice, at) VALUES ('chr/s/again', 'n1', 'b', 5), ('chr/s/again', 'n2', 'b', 6), ('chr/s/again', 'n3', 'b', 7)").run();
  db.prepare("UPDATE tasks SET n = 3, settled = 1, outcome = 'b', outcome_withdrawn = NULL, withdrawn_reason = NULL, withdrawn_at = NULL WHERE id = 'chr/s/again'").run();
  assert.deepEqual((await mark("merged_line", "chr/s/again")).changed, ["chr/s/again"]);
  assert.deepEqual(row("chr/s/again"), { n: 0, settled: 0, outcome: null, confusable_display: 0, held: 1, mark_reason: "merged_line", outcome_withdrawn: "b", withdrawn_reason: "merged_line" });
  assert.equal(db.prepare("SELECT COUNT(*) AS n FROM answers WHERE task_id = 'chr/s/again'").get().n, 0);
  assert.equal(db.prepare("SELECT COUNT(*) AS n FROM answers_excluded WHERE task_id = 'chr/s/again'").get().n, 6, "the old and the new answers are all kept");

  // a person who answered before and again after the reopening has two different answers and one place for them: refused, nothing changes
  await adminCall(DB, "/admin/confusable", { action: "reopen", tasks: ["chr/s/clash"] });
  db.prepare("INSERT INTO answers (task_id, contributor, choice, at) VALUES ('chr/s/clash', 'c1', 'neither', 9)").run();
  db.prepare("UPDATE tasks SET n = 1 WHERE id = 'chr/s/clash'").run();
  assert.deepEqual((await mark("merged_line", "chr/s/clash")).unchanged, ["chr/s/clash"]);
  assert.deepEqual([row("chr/s/clash").mark_reason, row("chr/s/clash").held, db.prepare("SELECT COUNT(*) AS n FROM answers WHERE task_id = 'chr/s/clash'").get().n, db.prepare("SELECT COUNT(*) AS n FROM answers_excluded WHERE task_id = 'chr/s/clash'").get().n], ["confusable_display", 0, 1, 3]);

  // settled again after already holding a withdrawn outcome: the second would overwrite the first, so it is refused too
  await adminCall(DB, "/admin/confusable", { action: "reopen", tasks: ["chr/s/twice"] });
  db.prepare("INSERT INTO answers (task_id, contributor, choice, at) VALUES ('chr/s/twice', 'n1', 'b', 5)").run();
  db.prepare("UPDATE tasks SET n = 3, settled = 1, outcome = 'b' WHERE id = 'chr/s/twice'").run();
  assert.deepEqual((await mark("merged_line", "chr/s/twice")).unchanged, ["chr/s/twice"]);
  assert.deepEqual([row("chr/s/twice").outcome, row("chr/s/twice").outcome_withdrawn, row("chr/s/twice").mark_reason], ["b", "a", "confusable_display"]);
});

// ---------- words held without a mark ----------

test("holding a task keeps its answers, counts and outcome, takes it out of the queue, and release undoes only the hold", async () => {
  const { db, DB } = await sqliteD1();
  const E = { ...env, DB };
  const get = async (path, token) => worker.fetch(new Request("https://check.usnewsmap.com" + path, { headers: token ? { authorization: "Bearer " + token } : {} }), E, {});
  const post = async (path, token, body) => worker.fetch(new Request("https://check.usnewsmap.com" + path, { method: "POST", headers: { authorization: "Bearer " + token }, body: JSON.stringify(body) }), E, {});
  const body = JSON.stringify({ page: "p", a: "ᏒᎯ", b: "ᎡᎯ", a_is: "reference", b_is: "loc" });
  for (const id of ["chr/s/1", "chr/s/2", "chr/s/3", "chr/s/4", "chr/s/5"]) db.prepare("INSERT INTO tasks (id, lang, set_name, body) VALUES (?, 'chr', 's', ?)").run(id, body);
  const [t1, t2, t3] = await Promise.all(["c1", "c2", "c3"].map((c) => signSession(env.SESSION_KEY, c)));
  for (const [tok, choice] of [[t1, "a"], [t2, "a"], [t3, "b"]]) await post("/api/answers", tok, { task: "chr/s/1", choice }); // settles: a
  await post("/api/answers", t1, { task: "chr/s/2", choice: "b" }); // unsettled
  const full = (id) => ({ ...db.prepare("SELECT * FROM tasks WHERE id = ?").get(id) });
  const answers = () => db.prepare("SELECT task_id, contributor, choice, at FROM answers ORDER BY task_id, contributor").all().map((r) => ({ ...r }));
  const stats = async () => JSON.stringify(await (await get("/api/stats?lang=chr")).json());
  const exported = async () => (await (await get("/api/export?lang=chr")).text()).trim().split("\n").map((l) => JSON.parse(l));
  const beforeRows = await exported();
  const beforeStats = await stats();
  const beforeAnswers = answers();
  const beforeTask = { 1: full("chr/s/1"), 2: full("chr/s/2") };
  // a task marked merged_line, and one marked confusable_display, for the refusals
  await adminCall(DB, "/admin/confusable", { action: "mark", reason: "merged_line", tasks: ["chr/s/3"] });
  await adminCall(DB, "/admin/confusable", { action: "mark", tasks: ["chr/s/4"] });

  const excludedBefore = db.prepare("SELECT * FROM answers_excluded ORDER BY task_id, contributor").all().map((r) => ({ ...r }));
  // the reason is required and checked
  for (const reason of [undefined, "", "Bad", "has space", "x".repeat(41), 1, null, "merged_line_"]) {
    assert.equal((await adminCall(DB, "/admin/confusable", { action: "hold", tasks: ["chr/s/1"], reason })).status, 400, String(reason));
  }
  assert.equal((await adminCall(DB, "/admin/confusable", { action: "hold", tasks: ["chr/s/1"], reason: "x".repeat(40) })).status, 200);
  assert.equal((await adminCall(DB, "/admin/confusable", { action: "release", tasks: ["chr/s/1"] })).status, 200);
  assert.deepEqual(full("chr/s/1"), beforeTask[1], "a 40-character slug is accepted, and release restores the row exactly");

  const held = await (await adminCall(DB, "/admin/confusable", { action: "hold", reason: "reference-mismatch-v1", tasks: ["chr/s/1", "chr/s/2", "chr/s/1", "chr/s/3", "chr/s/4", "chr/s/missing"] })).json();
  assert.deepEqual(held, { action: "hold", reason: "reference-mismatch-v1", requested: 5, changed: ["chr/s/1", "chr/s/2"], unchanged: ["chr/s/3", "chr/s/4"], missing: ["chr/s/missing"] });
  // the language list leaves held words out of both figures (chr/s/1 was settled and is held; 3 and 4 are marked and held)
  db.prepare("INSERT OR IGNORE INTO languages (code, config) VALUES ('chr', ?)").run(JSON.stringify({ name: "Cherokee" }));
  const card = (await (await get("/api/languages")).json()).find((l) => l.code === "chr");
  assert.deepEqual([card.tasks, card.settled, card.held], [1, 0, 4]);
  // only held and the reason changed; answers, counts, outcome, settled and everything else did not
  for (const [k, id] of [[1, "chr/s/1"], [2, "chr/s/2"]]) {
    assert.deepEqual({ ...full(id), held: 0, hold_reason: null }, beforeTask[k]);
    assert.deepEqual([full(id).held, full(id).hold_reason, full(id).withdrawn_reason], [1, "reference-mismatch-v1", null]);
  }
  assert.deepEqual(answers(), beforeAnswers);
  assert.deepEqual(db.prepare("SELECT * FROM answers_excluded ORDER BY task_id, contributor").all().map((r) => ({ ...r })), excludedBefore);
  // a second hold, even with another reason, changes nothing
  const again = await (await adminCall(DB, "/admin/confusable", { action: "hold", reason: "other-reason", tasks: ["chr/s/1"] })).json();
  assert.deepEqual([again.changed, again.unchanged], [[], ["chr/s/1"]]);
  assert.equal(full("chr/s/1").hold_reason, "reference-mismatch-v1");

  // the queue skips it; an answer gets the 409; the totals are unchanged
  const fresh = await signSession(env.SESSION_KEY, "fresh");
  const seen = new Set();
  for (let i = 0; i < 6; i++) { const r = await (await get("/api/next?lang=chr", fresh)).json(); if (r.task) seen.add(r.task.id); }
  assert.equal(seen.has("chr/s/1") || seen.has("chr/s/2"), false);
  assert.equal((await post("/api/answers", fresh, { task: "chr/s/1", choice: "a" })).status, 409);
  assert.equal((await post("/api/answers", fresh, { task: "chr/s/2", choice: "a" })).status, 409);
  assert.equal(await stats(), beforeStats);
  assert.deepEqual(answers(), beforeAnswers);

  // the export: the same fields plus held and hold_reason on the two held-only tasks; others unchanged
  const rows = await exported();
  const byId = (rs, id) => rs.find((x) => x.id === id);
  for (const id of ["chr/s/1", "chr/s/2"]) {
    const { held: h, hold_reason: why, ...rest } = byId(rows, id);
    assert.deepEqual([h, why], [true, "reference-mismatch-v1"]);
    assert.deepEqual(rest, byId(beforeRows, id));
  }
  const x1 = byId(rows, "chr/s/1");
  assert.deepEqual([x1.settled, x1.outcome, x1.counts.a, x1.counts.b], [true, "a", 2, 1]);
  assert.equal("mark_reason" in x1 || "excluded_counts" in x1, false);
  assert.equal("hold_reason" in byId(rows, "chr/s/3") || "hold_reason" in byId(rows, "chr/s/5"), false);
  assert.equal("held" in byId(rows, "chr/s/5"), false);
  assert.deepEqual(byId(rows, "chr/s/3").mark_reason, "merged_line");

  // release: undoes a hold only; a marked task is refused and left as it is
  const marked3 = full("chr/s/3"), marked4 = full("chr/s/4");
  const rel = await (await adminCall(DB, "/admin/confusable", { action: "release", tasks: ["chr/s/1", "chr/s/3", "chr/s/4", "chr/s/5", "chr/s/missing"] })).json();
  assert.deepEqual(rel, { action: "release", requested: 5, changed: ["chr/s/1"], unchanged: ["chr/s/3", "chr/s/4", "chr/s/5"], missing: ["chr/s/missing"] });
  assert.deepEqual([full("chr/s/3"), full("chr/s/4")], [marked3, marked4]);
  assert.deepEqual(full("chr/s/1"), beforeTask[1]);
  assert.equal((await post("/api/answers", fresh, { task: "chr/s/1", choice: "a" })).status, 200, "released: takes answers again");
  // a held word that was held for another cause (no hold reason) is not released
  db.prepare("UPDATE tasks SET held = 1 WHERE id = 'chr/s/5'").run();
  assert.deepEqual((await (await adminCall(DB, "/admin/confusable", { action: "release", tasks: ["chr/s/5"] })).json()).unchanged, ["chr/s/5"]);
  assert.deepEqual((await (await adminCall(DB, "/admin/confusable", { action: "hold", reason: "x", tasks: ["chr/s/5"] })).json()).unchanged, ["chr/s/5"]);
  // reopen does not release a hold
  assert.deepEqual((await (await adminCall(DB, "/admin/confusable", { action: "reopen", tasks: ["chr/s/2"] })).json()).unchanged, ["chr/s/2"]);
  assert.equal(full("chr/s/2").held, 1);
});

// ---------- column gutter judgments (#2107) ----------

const gutterBody = (extra = {}) => ({ kind: "gutter", crop: "en-gutter/g1/crops/p_s1.jpg", line: { x: 0.5, y0: 0.1, y1: 0.9 }, source: "https://example.test/p",
  private: { role: "pilot", segment_id: "s1", page_id: "p", method: "projection", truth: "yes", ...extra } });

test("a gutter task takes yes, no and can't tell, and refuses neither and not text; other tasks still take all five", async () => {
  const { db, DB } = await sqliteD1();
  const E = { ...env, DB };
  db.prepare("INSERT INTO tasks (id, lang, set_name, body) VALUES ('en-gutter/g1/p~s1', 'en-gutter', 'g1', ?), ('en/s/1', 'en', 's', '{\"a\":\"x\",\"b\":\"y\"}')").run(JSON.stringify(gutterBody()));
  const post = async (who, body) => worker.fetch(new Request("https://check.usnewsmap.com/api/answers", { method: "POST", headers: { authorization: "Bearer " + (await signSession(env.SESSION_KEY, who)) }, body: JSON.stringify(body) }), E, {});
  for (const choice of ["neither", "notext"]) {
    const r = await post("c1", { task: "en-gutter/g1/p~s1", choice });
    assert.equal(r.status, 400, choice);
    assert.match((await r.json()).error, /yes, no or can't tell/);
  }
  assert.equal(db.prepare("SELECT COUNT(*) AS k FROM answers").get().k, 0, "a refused answer is not stored");
  for (const [who, choice] of [["c1", "a"], ["c2", "b"], ["c3", "unsure"]]) assert.equal((await post(who, { task: "en-gutter/g1/p~s1", choice })).status, 200, choice);
  assert.equal((await post("c4", { task: "en/s/1", choice: "neither" })).status, 200);
  assert.equal((await post("c5", { task: "en/s/1", choice: "notext" })).status, 200);
});

test("a gutter task's private fields stay off /api/next and the export until it settles, then the export adds kind, line, source and what was private", async () => {
  const { db, DB } = await sqliteD1();
  const E = { ...env, DB };
  const load = await adminCall(DB, "/admin/tasks", null, [
    { id: "en-gutter/g1/p~s1", lang: "en-gutter", set: "g1", ...gutterBody() },
    { id: "en-gutter/g1/p~s2", lang: "en-gutter", set: "g1", ...gutterBody({ segment_id: "s2", method: "valley" }) },
  ].map((t) => JSON.stringify(t)).join("\n"));
  assert.equal(load.status, 200);
  const token = await signSession(env.SESSION_KEY, "viewer");
  const next = (await (await worker.fetch(new Request("https://check.usnewsmap.com/api/next?lang=en-gutter", { headers: { authorization: "Bearer " + token } }), E, {})).json()).task;
  assert.equal(next.kind, "gutter");
  assert.deepEqual(next.line, { x: 0.5, y0: 0.1, y1: 0.9 });
  assert.equal(next.crop, "en-gutter/g1/crops/p_s1.jpg");
  for (const secret of ["projection", "valley", "pilot", "truth", "private", "segment_id", "method"]) assert.equal(JSON.stringify(next).includes(secret), false, secret);
  const post = async (who, task, choice) => worker.fetch(new Request("https://check.usnewsmap.com/api/answers", { method: "POST", headers: { authorization: "Bearer " + (await signSession(env.SESSION_KEY, who)) }, body: JSON.stringify({ task, choice }) }), E, {});
  const exported = async () => (await (await worker.fetch(new Request("https://check.usnewsmap.com/api/export?lang=en-gutter"), E, {})).text()).trim().split("\n").map((l) => JSON.parse(l));
  await post("c1", "en-gutter/g1/p~s1", "a");
  await post("c2", "en-gutter/g1/p~s1", "a");
  let [open] = await exported();
  assert.deepEqual([open.kind, open.line, open.source, open.settled], ["gutter", { x: 0.5, y0: 0.1, y1: 0.9 }, "https://example.test/p", false]);
  for (const k of ["method", "role", "segment_id", "page_id", "truth", "private"]) assert.equal(k in open, false, `${k} is hidden while open`);
  assert.deepEqual(open.counts, { a: 2, b: 0, neither: 0, unsure: 0, notext: 0 });
  await post("c3", "en-gutter/g1/p~s1", "unsure");
  const [done, other] = await exported();
  assert.deepEqual([done.settled, done.outcome], [true, "a"]);
  assert.deepEqual([done.method, done.role, done.segment_id, done.page_id, done.truth], ["projection", "pilot", "s1", "p", "yes"]);
  assert.equal(other.settled, false);
  assert.equal("method" in other, false, "another open task stays hidden");
  // a settled task takes no more answers
  assert.equal((await post("c4", "en-gutter/g1/p~s1", "b")).status, 409);
  assert.equal(db.prepare("SELECT n FROM tasks WHERE id = 'en-gutter/g1/p~s1'").get().n, 3);
});

test("a gutter line may lean: x1 (its x at y1) is kept on /api/next and the export, and a line outside the crop is refused", async () => {
  const { db, DB } = await sqliteD1();
  const E = { ...env, DB };
  const lean = { x: 0.5, y0: 0.1, y1: 0.9, x1: 0.47 };
  const load = await adminCall(DB, "/admin/tasks", null, JSON.stringify({ id: "en-gutter/g1/p~s1", lang: "en-gutter", set: "g1", ...gutterBody(), line: lean }));
  assert.equal(load.status, 200);
  const token = await signSession(env.SESSION_KEY, "viewer");
  const next = (await (await worker.fetch(new Request("https://check.usnewsmap.com/api/next?lang=en-gutter", { headers: { authorization: "Bearer " + token } }), E, {})).json()).task;
  assert.deepEqual(next.line, lean);
  const [row] = (await (await worker.fetch(new Request("https://check.usnewsmap.com/api/export?lang=en-gutter"), E, {})).text()).trim().split("\n").map((l) => JSON.parse(l));
  assert.deepEqual(row.line, lean);
  for (const line of [{ ...lean, x1: 1.2 }, { ...lean, x1: -0.1 }, { ...lean, x1: "0.4" }, { ...lean, x1: null }, { ...lean, x: 2 }, { x: 0.5, y0: 0.1 }, undefined, [0.5, 0.1, 0.9]]) {
    const r = await adminCall(DB, "/admin/tasks", null, JSON.stringify({ id: "en-gutter/g1/p~s2", lang: "en-gutter", set: "g1", ...gutterBody({ segment_id: "s2" }), line }));
    assert.equal(r.status, 400, JSON.stringify(line));
    assert.match((await r.json()).error, /^en-gutter\/g1\/p~s2: .*line/);
  }
  const r = await adminCall(DB, "/admin/tasks", null, JSON.stringify({ id: "en-gutter/g1/p~s1", lang: "en-gutter", set: "g1", if_body: "x", ...gutterBody(), line: { ...lean, x1: 3 } }));
  assert.equal(r.status, 400, "a conditional replacement is checked too");
  assert.equal(db.prepare("SELECT COUNT(*) AS k FROM tasks").get().k, 1, "a refused line stores nothing");
  assert.deepEqual(JSON.parse(db.prepare("SELECT body FROM tasks").get().body).line, lean);
});

test("a leaning gutter line refuses answers from a page that draws it vertical; a vertical one takes them from any page", async () => {
  const { db, DB } = await sqliteD1();
  const E = { ...env, DB };
  const load = await adminCall(DB, "/admin/tasks", null, [
    { id: "en-gutter/g1/p~s1", lang: "en-gutter", set: "g1", ...gutterBody(), line: { x: 0.5, y0: 0.1, y1: 0.9, x1: 0.47 } },
    { id: "en-gutter/g1/p~s2", lang: "en-gutter", set: "g1", ...gutterBody({ segment_id: "s2" }) },
  ].map((t) => JSON.stringify(t)).join("\n"));
  assert.equal(load.status, 200);
  const post = async (who, body) => worker.fetch(new Request("https://check.usnewsmap.com/api/answers", { method: "POST", headers: { authorization: "Bearer " + (await signSession(env.SESSION_KEY, who)) }, body: JSON.stringify(body) }), E, {});
  assert.ok(DISPLAY_REVISION >= 2, "the revision that draws x1");
  for (const display of [undefined, 1, DISPLAY_REVISION - 1]) assert.equal((await post("c1", { task: "en-gutter/g1/p~s1", choice: "a", display })).status, 426, String(display));
  assert.equal(db.prepare("SELECT COUNT(*) AS k FROM answers").get().k, 0, "a stale page's answer is not stored");
  assert.equal((await post("c1", { task: "en-gutter/g1/p~s1", choice: "a", display: DISPLAY_REVISION })).status, 200);
  assert.equal((await post("c1", { task: "en-gutter/g1/p~s2", choice: "a", display: 1 })).status, 200, "a vertical line is drawn the same by every page");
  // the statement itself refuses a stale page too, against a request that read the task before its line was made to lean
  const { ANSWER_SQL } = await import("./worker.mjs");
  assert.equal(db.prepare(ANSWER_SQL).run("en-gutter/g1/p~s1", "c2", "a", 0, 1, "unknown", "unknown").changes, 0);
  assert.equal(db.prepare(ANSWER_SQL).run("en-gutter/g1/p~s2", "c2", "a", 0, 1, "unknown", "unknown").changes, 1);
  assert.equal(db.prepare(ANSWER_SQL).run("en-gutter/g1/p~s1", "c2", "a", 0, DISPLAY_REVISION, "unknown", "unknown").changes, 1);
});
