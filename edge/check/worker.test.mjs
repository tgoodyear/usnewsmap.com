// node --test edge/check/*.test.mjs
import test from "node:test";
import assert from "node:assert/strict";
import worker, { settle, signSession, verifySession } from "./worker.mjs";

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
  const data = { languages: [{ code: "chr", config: "{}" }], tasks: [{ id: "t1" }, { id: "t2" }], answers: [{ task_id: "t1", contributor: "c", choice: "a", at: 1 }] };
  const DB = { prepare: (sql) => ({ bind: (offset) => ({ all: async () => {
    const table = sql.match(/FROM (\w+)/)[1];
    return { results: offset === 0 ? data[table] : [] };
  } }) }) };
  const r = await worker.fetch(new Request("https://check.usnewsmap.com/admin/dump", { headers: { authorization: "Bearer secret-admin" } }), { ...env, DB }, {});
  assert.equal(r.status, 200);
  const lines = (await r.text()).trim().split("\n").map((l) => JSON.parse(l));
  assert.deepEqual(lines.map((l) => l.table), ["languages", "tasks", "tasks", "answers"]);
  assert.equal(lines[3].row.choice, "a");
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
