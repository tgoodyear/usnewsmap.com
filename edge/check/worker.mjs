// check.usnewsmap.com: people compare a printed word with two OCR readings and say which
// matches (#1620). The page is static (public/); this Worker answers /api/*, /clips/* and
// /admin/*. README.md describes the data, the settling rule and how tasks are loaded.

const CHOICES = new Set(["a", "b", "neither", "unsure", "notext"]);
const SESSION_DAYS = 30;
const SETTLE_MIN = 3; // answers before a word can settle
const DISPUTE_AT = 7; // answers after which a word that never agreed is closed as disputed
const enc = new TextEncoder();

export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);
    try {
      if (url.pathname.startsWith("/api/")) return await api(request, env, url);
      if (url.pathname.startsWith("/clips/")) return await clip(request, env, url, ctx);
      if (url.pathname.startsWith("/admin/")) return await admin(request, env, url);
      return env.ASSETS.fetch(request);
    } catch (e) {
      if (e instanceof HttpError) return json({ error: e.message }, e.status);
      console.error("unhandled", e && e.stack ? e.stack : String(e));
      return json({ error: "internal error" }, 500);
    }
  },
};

export class HttpError extends Error {
  constructor(status, message) {
    super(message);
    this.status = status;
  }
}

export function json(body, status = 200, headers = {}) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json; charset=utf-8", "cache-control": "no-store", ...headers },
  });
}

// ---------- the settling rule ----------

// counts: {a, b, neither, unsure, notext}, in that order: a tie goes to the first. A word settles once at least SETTLE_MIN people answered and
// one answer has two thirds of them. After DISPUTE_AT answers without that, it closes as disputed.
export function settle(counts) {
  const n = Object.values(counts).reduce((s, v) => s + v, 0);
  let top = null;
  for (const [k, v] of Object.entries(counts)) if (top === null || v > counts[top]) top = k;
  if (n >= SETTLE_MIN && top !== null && counts[top] * 3 >= n * 2) return { n, settled: 1, outcome: top };
  if (n >= DISPUTE_AT) return { n, settled: 1, outcome: "disputed" };
  return { n, settled: 0, outcome: null };
}

// ---------- sessions ----------

function b64url(bytes) {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

function unb64url(s) {
  const bin = atob(s.replaceAll("-", "+").replaceAll("_", "/"));
  return Uint8Array.from(bin, (c) => c.charCodeAt(0));
}

async function hmacKey(secret) {
  return crypto.subtle.importKey("raw", enc.encode(secret), { name: "HMAC", hash: "SHA-256" }, false, ["sign", "verify"]);
}

export async function signSession(secret, contributor, now = Date.now()) {
  const payload = b64url(enc.encode(JSON.stringify({ c: contributor, e: now + SESSION_DAYS * 86400e3 })));
  const sig = new Uint8Array(await crypto.subtle.sign("HMAC", await hmacKey(secret), enc.encode(payload)));
  return payload + "." + b64url(sig);
}

export async function verifySession(secret, token, now = Date.now()) {
  if (typeof token !== "string" || token.length > 400) return null;
  const [payload, sig] = token.split(".");
  if (!payload || !sig) return null;
  let ok = false;
  try {
    ok = await crypto.subtle.verify("HMAC", await hmacKey(secret), unb64url(sig), enc.encode(payload));
  } catch {
    return null;
  }
  if (!ok) return null;
  let body;
  try {
    body = JSON.parse(new TextDecoder().decode(unb64url(payload)));
  } catch {
    return null;
  }
  if (typeof body.c !== "string" || typeof body.e !== "number" || body.e < now) return null;
  return body.c;
}

async function contributorOf(request, env, { limited = false } = {}) {
  const auth = request.headers.get("authorization") || "";
  const c = auth.startsWith("Bearer ") ? await verifySession(env.SESSION_KEY, auth.slice(7)) : null;
  if (!c) throw new HttpError(401, "session required");
  if (limited && env.ANSWER_LIMIT) {
    const { success } = await env.ANSWER_LIMIT.limit({ key: c });
    if (!success) throw new HttpError(429, "too many answers; slow down");
  }
  return c;
}

async function turnstileOk(env, token, ip) {
  if (typeof token !== "string" || !token || token.length > 4096) return false;
  const form = new FormData();
  form.append("secret", env.TURNSTILE_SECRET);
  form.append("response", token);
  if (ip) form.append("remoteip", ip);
  const r = await fetch("https://challenges.cloudflare.com/turnstile/v0/siteverify", { method: "POST", body: form });
  const out = await r.json();
  return out.success === true;
}

// ---------- public API ----------

// Reads at most `max` bytes: a declared or streamed body past that is refused before it is
// buffered, so an anonymous request can't make the Worker hold a large body in memory.
export async function readLimited(request, max) {
  const declared = Number(request.headers.get("content-length"));
  if (declared > max) throw new HttpError(413, "body too large");
  if (!request.body) return "";
  const reader = request.body.getReader();
  const chunks = [];
  let size = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    size += value.byteLength;
    if (size > max) {
      await reader.cancel();
      throw new HttpError(413, "body too large");
    }
    chunks.push(value);
  }
  const all = new Uint8Array(size);
  let at = 0;
  for (const c of chunks) { all.set(c, at); at += c.byteLength; }
  return new TextDecoder().decode(all);
}

async function body(request, max = 4096) {
  const text = await readLimited(request, max);
  try {
    return JSON.parse(text);
  } catch {
    throw new HttpError(400, "body must be JSON");
  }
}

function lang(url) {
  const l = url.searchParams.get("lang") || "";
  if (!/^[a-z]{2,3}(-[A-Za-z0-9]{2,8})*$/.test(l)) throw new HttpError(400, "lang required");
  return l;
}

// A task's body as the page gets it. `private` (blind tasks: which reading is which, the truth for a
// calibration item, the sampling strata) stays on the server until the task has settled and been exported.
export function shownBody(text) {
  const { private: _hidden, ...shown } = JSON.parse(text);
  return shown;
}

async function api(request, env, url) {
  const p = url.pathname;
  if (p === "/api/config" && request.method === "GET") {
    return json({ turnstile_site_key: env.TURNSTILE_SITE_KEY || "" }, 200, { "cache-control": "public, max-age=300" });
  }
  if (p === "/api/languages" && request.method === "GET") {
    const rows = await env.DB.prepare(
      "SELECT l.code, l.config, COUNT(t.id) AS tasks, SUM(t.settled) AS settled FROM languages l LEFT JOIN tasks t ON t.lang = l.code GROUP BY l.code ORDER BY l.code",
    ).all();
    return json(rows.results.map((r) => ({ code: r.code, name: JSON.parse(r.config).name, tasks: r.tasks, settled: r.settled || 0 })), 200, {
      "cache-control": "public, max-age=60",
    });
  }
  const m = p.match(/^\/api\/languages\/([a-z]{2,3}(?:-[A-Za-z0-9]{2,8})*)$/);
  if (m && request.method === "GET") {
    const row = await env.DB.prepare("SELECT config FROM languages WHERE code = ?").bind(m[1]).first();
    if (!row) throw new HttpError(404, "no such language");
    return new Response(row.config, { headers: { "content-type": "application/json; charset=utf-8", "cache-control": "public, max-age=300" } });
  }
  if (p === "/api/session" && request.method === "POST") {
    const b = await body(request);
    if (!(await turnstileOk(env, b.turnstile, request.headers.get("cf-connecting-ip")))) throw new HttpError(403, "the browser check failed; reload and try again");
    const c = b64url(crypto.getRandomValues(new Uint8Array(16)));
    return json({ session: await signSession(env.SESSION_KEY, c) });
  }
  if (p === "/api/next" && request.method === "GET") {
    const c = await contributorOf(request, env);
    const l = lang(url);
    const skip = (url.searchParams.get("skip") || "").split(",").filter(Boolean).slice(0, 50);
    const holes = skip.map(() => "?").join(",");
    const row = await env.DB.prepare(
      `SELECT id, body, n FROM tasks t WHERE lang = ? AND settled = 0 AND held = 0
         AND NOT EXISTS (SELECT 1 FROM answers a WHERE a.task_id = t.id AND a.contributor = ?)
         ${skip.length ? `AND id NOT IN (${holes})` : ""}
       ORDER BY n DESC, random() LIMIT 1`,
    )
      .bind(l, c, ...skip)
      .first();
    const mine = await env.DB.prepare("SELECT COUNT(*) AS k FROM answers WHERE contributor = ?").bind(c).first();
    return json({ task: row ? { id: row.id, ...shownBody(row.body) } : null, answered: mine.k });
  }
  if (p === "/api/answers" && request.method === "POST") {
    const c = await contributorOf(request, env, { limited: true });
    const b = await body(request);
    if (typeof b.task !== "string" || b.task.length > 300 || !CHOICES.has(b.choice)) throw new HttpError(400, "task and choice required");
    const t = await env.DB.prepare("SELECT id, settled, held, json_type(body, '$.private') AS blind FROM tasks WHERE id = ?").bind(b.task).first();
    if (!t) throw new HttpError(404, "no such task");
    if (t.settled && t.blind) throw new HttpError(409, "this word is already checked");
    if (t.held) throw new HttpError(409, "this word is not open for answers");
    await record(env, b.task, c, b.choice);
    return json({ ok: true });
  }
  if (p === "/api/stats" && request.method === "GET") {
    const l = lang(url);
    const r = await env.DB.prepare(
      "SELECT COUNT(*) AS tasks, SUM(settled) AS settled, SUM(n) AS answers FROM tasks WHERE lang = ?",
    )
      .bind(l)
      .first();
    const people = await env.DB.prepare(
      "SELECT COUNT(DISTINCT a.contributor) AS k FROM answers a JOIN tasks t ON t.id = a.task_id WHERE t.lang = ?",
    )
      .bind(l)
      .first();
    return json({ tasks: r.tasks, settled: r.settled || 0, answers: r.answers || 0, people: people.k }, 200, { "cache-control": "public, max-age=60" });
  }
  if (p === "/api/export" && request.method === "GET") {
    const l = lang(url);
    const rows = await env.DB.prepare(
      `SELECT t.id, t.set_name, t.body, t.outcome, t.settled, t.confusable_display, t.held,
         SUM(a.choice = 'a') AS a, SUM(a.choice = 'b') AS b, SUM(a.choice = 'neither') AS neither, SUM(a.choice = 'unsure') AS unsure,
         SUM(a.choice = 'notext') AS notext
       FROM tasks t LEFT JOIN answers a ON a.task_id = t.id WHERE t.lang = ? GROUP BY t.id ORDER BY t.id`,
    )
      .bind(l)
      .all();
    // Answers set aside for a task marked confusable_display (README.md, "Words whose readings look alike"): counted apart.
    const set = await env.DB.prepare(
      `SELECT e.task_id, COUNT(*) AS n, SUM(e.choice = 'a') AS a, SUM(e.choice = 'b') AS b, SUM(e.choice = 'neither') AS neither,
         SUM(e.choice = 'unsure') AS unsure, SUM(e.choice = 'notext') AS notext
       FROM answers_excluded e JOIN tasks t ON t.id = e.task_id WHERE t.lang = ? GROUP BY e.task_id`,
    )
      .bind(l)
      .all();
    const excluded = new Map(set.results.map((e) => [e.task_id, { a: e.a || 0, b: e.b || 0, neither: e.neither || 0, unsure: e.unsure || 0, notext: e.notext || 0 }]));
    const lines = rows.results.map((r) => {
      const t = JSON.parse(r.body);
      // A blind task keeps what each reading is, and the truth for a calibration item, in `private`: they are
      // exported once the task has settled, never before.
      const reveal = t.private && r.settled ? t.private : {};
      return JSON.stringify({
        id: r.id, set: r.set_name, page: t.page, source: t.source, a: t.a, b: t.b,
        a_is: t.private ? reveal.a_is : t.a_is, b_is: t.private ? reveal.b_is : t.b_is,
        ...(t.private ? { strata: t.private.strata, truth: reveal.truth } : {}),
        ...(t.lookalikes ? { lookalikes: t.lookalikes } : {}),
        counts: { a: r.a || 0, b: r.b || 0, neither: r.neither || 0, unsure: r.unsure || 0, notext: r.notext || 0 },
        // confusable_display: the readings looked alike in the visitor's font. The answers given before are in
        // excluded_counts, never in counts, and they don't settle the word or count in any score. `held` is true until the
        // word is reopened for new answers.
        ...(r.confusable_display ? { confusable_display: true, held: !!r.held, excluded_counts: excluded.get(r.id) || { a: 0, b: 0, neither: 0, unsure: 0, notext: 0 } } : {}),
        settled: !!r.settled, outcome: r.outcome,
      });
    });
    return new Response(lines.join("\n") + "\n", {
      headers: {
        "content-type": "application/x-ndjson; charset=utf-8",
        "content-disposition": `attachment; filename="check-${l}.jsonl"`,
        "cache-control": "public, max-age=300",
        "x-license": "CC0-1.0",
      },
    });
  }
  throw new HttpError(404, "not found");
}

// Recounts a word's answers and applies settle()'s rule in SQL, so the upsert and the recount run
// in one D1 batch (one transaction) and a slower request can't write back stale counts.
// worker.test.mjs checks it against settle() on SQLite.
// The language's new configuration (?1) with the stored examples, if it has any, put back in.
export const KEEP_EXAMPLES_SQL = `
UPDATE languages SET config = CASE
  WHEN json_type(config, '$.examples') IS NULL THEN json(?1)
  ELSE json_set(json(?1), '$.examples', json(json_extract(config, '$.examples')))
END
WHERE code = ?2`;

export const SETTLE_SQL = `
WITH c AS (
  SELECT COUNT(*) AS n,
    COALESCE(SUM(choice = 'a'), 0) AS a, COALESCE(SUM(choice = 'b'), 0) AS b,
    COALESCE(SUM(choice = 'neither'), 0) AS ne, COALESCE(SUM(choice = 'unsure'), 0) AS u,
    COALESCE(SUM(choice = 'notext'), 0) AS nt
  FROM answers WHERE task_id = ?1
), t AS (
  SELECT n, MAX(a, b, ne, u, nt) AS top,
    CASE WHEN a >= b AND a >= ne AND a >= u AND a >= nt THEN 'a' WHEN b >= ne AND b >= u AND b >= nt THEN 'b'
      WHEN ne >= u AND ne >= nt THEN 'neither' WHEN u >= nt THEN 'unsure' ELSE 'notext' END AS pick
  FROM c
)
UPDATE tasks SET
  n = (SELECT n FROM t),
  settled = (SELECT CASE WHEN (n >= ${SETTLE_MIN} AND top * 3 >= n * 2) OR n >= ${DISPUTE_AT} THEN 1 ELSE 0 END FROM t),
  outcome = (SELECT CASE WHEN n >= ${SETTLE_MIN} AND top * 3 >= n * 2 THEN pick WHEN n >= ${DISPUTE_AT} THEN 'disputed' END FROM t)
WHERE id = ?1`;

// A settled blind task (its body has `private`) takes no more answers, so a revision can't reopen it after its
// readings were exported, and a held task (marked confusable_display, not yet reopened) takes none either. The
// conditions are in the statement, so they hold against a racing request too.
export const ANSWER_SQL = `
INSERT INTO answers (task_id, contributor, choice, at)
SELECT ?1, ?2, ?3, ?4
WHERE NOT EXISTS (SELECT 1 FROM tasks WHERE id = ?1 AND ((settled = 1 AND json_type(body, '$.private') IS NOT NULL) OR held = 1))
ON CONFLICT (task_id, contributor) DO UPDATE SET choice = excluded.choice, at = excluded.at`;

export async function record(env, task, contributor, choice, at = Date.now()) {
  const res = await env.DB.batch([
    env.DB.prepare(ANSWER_SQL).bind(task, contributor, choice, at),
    env.DB.prepare(SETTLE_SQL).bind(task),
  ]);
  // Zero rows written: the task settled (blind) or was held between the check in /api/answers and this statement.
  if (res && res[0] && res[0].meta && res[0].meta.changes === 0) throw new HttpError(409, "this word is already checked");
}

// ---------- words whose readings look alike (README.md, "Words whose readings look alike") ----------

// Marking a task confusable_display sets its existing answers aside, in one batch (one transaction) per task: they are
// copied to answers_excluded, the copy is checked against the original, the originals leave `answers` (so they settle
// nothing and count in no figure, and their authors may answer again) and the task is held out of the queue, unsettled,
// with its count at zero. Nothing is deleted without a copy; the statement that checks the copy fails the batch if a row
// is missing. Every statement is guarded by the task still being unmarked, inside the transaction, so a request that read
// the task before another marked it (and the task was reopened and took new answers) changes nothing. ?1 is the task id.
export const MARK_CONFUSABLE_SQL = [
  `INSERT INTO answers_excluded (task_id, contributor, choice, at) SELECT task_id, contributor, choice, at FROM answers
     WHERE task_id = ?1 AND EXISTS (SELECT 1 FROM tasks WHERE id = ?1 AND confusable_display = 0)`,
  `SELECT CASE WHEN EXISTS (SELECT 1 FROM tasks WHERE id = ?1 AND confusable_display = 0)
     AND (SELECT COUNT(*) FROM answers_excluded WHERE task_id = ?1) < (SELECT COUNT(*) FROM answers WHERE task_id = ?1)
     THEN json_extract('answers were not all copied', '$') END`,
  `DELETE FROM answers WHERE task_id = ?1 AND EXISTS (SELECT 1 FROM tasks WHERE id = ?1 AND confusable_display = 0)`,
  `UPDATE tasks SET confusable_display = 1, held = 1, n = 0, settled = 0, outcome = NULL WHERE id = ?1 AND confusable_display = 0`,
];
// Reopening lets a marked task take new answers again. Its excluded answers stay excluded.
export const REOPEN_CONFUSABLE_SQL = `UPDATE tasks SET held = 0 WHERE id = ?1 AND confusable_display = 1 AND held = 1`;

// Applies "mark" or "reopen" to task ids. Marking a task that is already marked does nothing (so a second run can't
// set aside the new answers); reopening a task that isn't held does nothing. `found` only tells which ids are tasks.
export async function confusable(env, action, ids) {
  const found = new Map();
  for (let i = 0; i < ids.length; i += 50) {
    const part = ids.slice(i, i + 50);
    const rows = await env.DB.prepare(`SELECT id, confusable_display, held FROM tasks WHERE id IN (${part.map(() => "?").join(",")})`).bind(...part).all();
    for (const r of rows.results) found.set(r.id, r);
  }
  const out = { action, requested: ids.length, changed: [], unchanged: [], missing: ids.filter((i) => !found.has(i)) };
  for (const id of ids) {
    if (!found.has(id)) continue;
    // changed or not is what the guarded UPDATE did, not what was read above
    let changes;
    if (action === "mark") {
      const res = await env.DB.batch(MARK_CONFUSABLE_SQL.map((sql) => env.DB.prepare(sql).bind(id)));
      changes = res[res.length - 1].meta.changes;
    } else {
      changes = (await env.DB.prepare(REOPEN_CONFUSABLE_SQL).bind(id).run()).meta.changes;
    }
    (changes ? out.changed : out.unchanged).push(id);
  }
  return out;
}

// ---------- clips ----------

// Clips never change once loaded (a new set gets new keys), so each one is kept in Cloudflare's edge cache
// for a year: a Worker's response isn't cached on its own, so without this every view is an R2 read.
async function clip(request, env, url, ctx) {
  if (request.method !== "GET" && request.method !== "HEAD") throw new HttpError(405, "method not allowed");
  const key = decodeURIComponent(url.pathname.slice("/clips/".length));
  if (!/^[A-Za-z0-9._\-/]{1,300}$/.test(key) || key.includes("..")) throw new HttpError(404, "not found");
  const cache = typeof caches !== "undefined" ? caches.default : null;
  const cacheKey = new Request(url.origin + url.pathname, { method: "GET" });
  let res = cache ? await cache.match(cacheKey) : undefined;
  if (!res) {
    const obj = await env.CLIPS.get(key);
    if (!obj) throw new HttpError(404, "not found");
    res = new Response(obj.body, {
      headers: {
        "content-type": obj.httpMetadata?.contentType || "image/jpeg",
        "cache-control": "public, max-age=31536000, immutable",
        etag: obj.httpEtag,
      },
    });
    if (cache && ctx) ctx.waitUntil(cache.put(cacheKey, res.clone()));
  }
  return request.method === "HEAD" ? new Response(null, { status: res.status, headers: res.headers }) : res;
}

// ---------- loading tasks (README.md, "Loading a language") ----------

function timingSafeEqual(a, b) {
  const x = enc.encode(a), y = enc.encode(b);
  let d = x.length ^ y.length;
  for (let i = 0; i < Math.max(x.length, y.length); i++) d |= (x[i] || 0) ^ (y[i] || 0);
  return d === 0;
}

async function admin(request, env, url) {
  const auth = request.headers.get("authorization") || "";
  if (!env.ADMIN_TOKEN || !auth.startsWith("Bearer ") || !timingSafeEqual(auth.slice(7), env.ADMIN_TOKEN)) throw new HttpError(401, "unauthorized");
  const p = url.pathname;
  let m;
  if ((m = p.match(/^\/admin\/clips\/(.+)$/)) && request.method === "PUT") {
    const key = decodeURIComponent(m[1]);
    if (!/^[A-Za-z0-9._\-/]{1,300}$/.test(key) || key.includes("..")) throw new HttpError(400, "bad key");
    const type = request.headers.get("content-type") || "image/jpeg";
    if (!/^image\/(jpeg|png|webp)$/.test(type)) throw new HttpError(400, "images only");
    await env.CLIPS.put(key, request.body, { httpMetadata: { contentType: type } });
    return json({ ok: true, key });
  }
  if ((m = p.match(/^\/admin\/languages\/([a-z]{2,3}(?:-[A-Za-z0-9]{2,8})*)$/)) && request.method === "PUT") {
    const cfg = await body(request, 200000);
    if (typeof cfg.name !== "string") throw new HttpError(400, "name required");
    // ?keep=examples (sync-languages.mjs): replace the rest but keep the stored examples, in one
    // statement, so pictures load.sh sends meanwhile aren't overwritten with an older copy.
    if (url.searchParams.get("keep") === "examples") {
      const r = await env.DB.prepare(KEEP_EXAMPLES_SQL).bind(JSON.stringify(cfg), m[1]).run();
      if (!r.meta?.changes) throw new HttpError(404, "no such language");
      return json({ ok: true });
    }
    await env.DB.prepare("INSERT INTO languages (code, config) VALUES (?, ?) ON CONFLICT (code) DO UPDATE SET config = excluded.config")
      .bind(m[1], JSON.stringify(cfg))
      .run();
    return json({ ok: true });
  }
  if (p === "/admin/tasks" && request.method === "POST") {
    // JSON lines: {id, lang, set, ...shown fields}. Existing tasks keep their answers; their body is replaced.
    const text = await request.text();
    const stmts = [];
    for (const line of text.split("\n")) {
      if (!line.trim()) continue;
      const t = JSON.parse(line);
      if (typeof t.id !== "string" || typeof t.lang !== "string" || typeof t.set !== "string") throw new HttpError(400, "id, lang and set required");
      const { id, lang: l, set, ...shown } = t;
      stmts.push(
        env.DB.prepare("INSERT INTO tasks (id, lang, set_name, body) VALUES (?, ?, ?, ?) ON CONFLICT (id) DO UPDATE SET body = excluded.body")
          .bind(id, l, set, JSON.stringify(shown)),
      );
    }
    for (let i = 0; i < stmts.length; i += 50) await env.DB.batch(stmts.slice(i, i + 50));
    return json({ ok: true, tasks: stmts.length });
  }
  if (p === "/admin/confusable" && request.method === "POST") {
    const b = await body(request, 200000);
    const ids = Array.isArray(b.tasks) ? [...new Set(b.tasks)] : [];
    if ((b.action !== "mark" && b.action !== "reopen") || !ids.length || ids.length > 500 || ids.some((i) => typeof i !== "string" || !i || i.length > 300)) {
      throw new HttpError(400, 'body wants {"action": "mark" or "reopen", "tasks": [1 to 500 task ids]}');
    }
    return json(await confusable(env, b.action, ids));
  }
  if (p === "/admin/dump" && request.method === "GET") {
    // Everything in D1 as JSON lines, one {"table", "row"} per line, for the nightly backup
    // (backup/README.md). The four reads run as one batch, which D1 runs as one transaction,
    // so tasks' counts and the answers come from the same moment and no row is read twice.
    const tables = [["languages", "code"], ["tasks", "id"], ["answers", "task_id, contributor"], ["answers_excluded", "task_id, contributor"]];
    const results = await env.DB.batch(tables.map(([table, order]) => env.DB.prepare(`SELECT * FROM ${table} ORDER BY ${order}`)));
    const out = [];
    tables.forEach(([table], k) => { for (const row of results[k].results) out.push(JSON.stringify({ table, row })); });
    return new Response(out.join("\n") + "\n", { headers: { "content-type": "application/x-ndjson; charset=utf-8", "cache-control": "no-store" } });
  }
  if (p === "/admin/answers" && request.method === "POST") {
    // JSON lines: {task, contributor ("seed:<name>"), choice, at}. Imported answers count like any other.
    const text = await request.text();
    let k = 0;
    for (const line of text.split("\n")) {
      if (!line.trim()) continue;
      const a = JSON.parse(line);
      if (typeof a.contributor !== "string" || !a.contributor.startsWith("seed:") || !CHOICES.has(a.choice)) throw new HttpError(400, "seed answers only");
      await record(env, a.task, a.contributor, a.choice, Number(a.at) || Date.parse(a.at) || Date.now());
      k++;
    }
    return json({ ok: true, answers: k });
  }
  throw new HttpError(404, "not found");
}
