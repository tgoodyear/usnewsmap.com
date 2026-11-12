// check.usnewsmap.com: people compare a printed word with two OCR readings and say which
// matches (#1620). The page is static (public/); this Worker answers /api/*, /clips/* and
// /admin/*. README.md describes the data, the settling rule and how tasks are loaded.

const CHOICES = new Set(["a", "b", "neither", "unsure", "notext"]);
const SESSION_DAYS = 30;
const SETTLE_MIN = 3; // answers before a word can settle
const DISPUTE_AT = 7; // answers after which a word that never agreed is closed as disputed
const enc = new TextEncoder();
import { MAX_LINES, MAX_TEXT, CAL_MAX_CER, cer, flip, interCer, normalizeReading, qualified, readerId, settleReadings, wantCalibration } from "./transcribe.mjs";

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

async function api(request, env, url) {
  const p = url.pathname;
  if (p === "/api/config" && request.method === "GET") {
    return json({ turnstile_site_key: env.TURNSTILE_SITE_KEY || "" }, 200, { "cache-control": "public, max-age=300" });
  }
  if (p === "/api/languages" && request.method === "GET") {
    const rows = await env.DB.prepare(
      "SELECT l.code, l.config, COUNT(t.id) AS tasks, SUM(t.settled) AS settled FROM languages l LEFT JOIN tasks t ON t.lang = l.code GROUP BY l.code HAVING COALESCE(json_extract(l.config, '$.unlisted'), 0) != 1 ORDER BY l.code",
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
    if (await isTranscribe(env, l)) {
      const cfg = await langConfig(env, l);
      if (cfg.readers_prefix && !c.startsWith(cfg.readers_prefix)) throw new HttpError(403, "this set is read by its maintainer only");
      return json(await nextReading(env, l, c, url));
    }
    const skip = (url.searchParams.get("skip") || "").split(",").filter(Boolean).slice(0, 50);
    const holes = skip.map(() => "?").join(",");
    const row = await env.DB.prepare(
      `SELECT id, body, n FROM tasks t WHERE lang = ? AND settled = 0
         AND NOT EXISTS (SELECT 1 FROM answers a WHERE a.task_id = t.id AND a.contributor = ?)
         ${skip.length ? `AND id NOT IN (${holes})` : ""}
       ORDER BY n DESC, random() LIMIT 1`,
    )
      .bind(l, c, ...skip)
      .first();
    const mine = await env.DB.prepare("SELECT COUNT(*) AS k FROM answers WHERE contributor = ?").bind(c).first();
    return json({ task: row ? { id: row.id, ...JSON.parse(row.body) } : null, answered: mine.k });
  }
  if (p === "/api/readings" && request.method === "POST") {
    const c = await contributorOf(request, env, { limited: true });
    const b = await body(request, 20000);
    if (typeof b.task !== "string" || b.task.length > 300 || typeof b.text !== "string") throw new HttpError(400, "task and text required");
    await requireReader(env, b.task, c);
    return json(await recordReading(env, b.task, c, b.text, b.unreadable === true));
  }
  if (p === "/api/answers" && request.method === "POST") {
    const c = await contributorOf(request, env, { limited: true });
    const b = await body(request);
    if (typeof b.task !== "string" || b.task.length > 300 || !CHOICES.has(b.choice)) throw new HttpError(400, "task and choice required");
    const t = await env.DB.prepare("SELECT id, body FROM tasks WHERE id = ?").bind(b.task).first();
    if (!t) throw new HttpError(404, "no such task");
    if (JSON.parse(t.body).kind === "transcribe") throw new HttpError(400, "this task takes a reading, not a choice");
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
    if (await isTranscribe(env, l)) return exportReadings(env, l);
    const rows = await env.DB.prepare(
      `SELECT t.id, t.set_name, t.body, t.outcome, t.settled,
         SUM(a.choice = 'a') AS a, SUM(a.choice = 'b') AS b, SUM(a.choice = 'neither') AS neither, SUM(a.choice = 'unsure') AS unsure,
         SUM(a.choice = 'notext') AS notext
       FROM tasks t LEFT JOIN answers a ON a.task_id = t.id WHERE t.lang = ? GROUP BY t.id ORDER BY t.id`,
    )
      .bind(l)
      .all();
    const lines = rows.results.map((r) => {
      const t = JSON.parse(r.body);
      return JSON.stringify({
        id: r.id, set: r.set_name, page: t.page, source: t.source, a: t.a, b: t.b, a_is: t.a_is, b_is: t.b_is,
        counts: { a: r.a || 0, b: r.b || 0, neither: r.neither || 0, unsure: r.unsure || 0, notext: r.notext || 0 },
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

export async function record(env, task, contributor, choice, at = Date.now()) {
  await env.DB.batch([
    env.DB.prepare(
      "INSERT INTO answers (task_id, contributor, choice, at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT (task_id, contributor) DO UPDATE SET choice = excluded.choice, at = excluded.at",
    ).bind(task, contributor, choice, at),
    env.DB.prepare(SETTLE_SQL).bind(task),
  ]);
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
      const { id, lang: l, set, truth, ...shown } = t;
      if (truth !== undefined) {
        // A calibration task: its known text goes in its own table and never reaches the page.
        if (typeof truth !== "string" || !truth) throw new HttpError(400, "truth must be text");
        shown.calibration = true;
        stmts.push(env.DB.prepare("INSERT INTO calibration (task_id, truth) VALUES (?, ?) ON CONFLICT (task_id) DO UPDATE SET truth = excluded.truth").bind(id, normalizeReading(truth)));
      }
      stmts.push(
        env.DB.prepare("INSERT INTO tasks (id, lang, set_name, body) VALUES (?, ?, ?, ?) ON CONFLICT (id) DO UPDATE SET body = excluded.body")
          .bind(id, l, set, JSON.stringify(shown)),
      );
    }
    for (let i = 0; i < stmts.length; i += 50) await env.DB.batch(stmts.slice(i, i + 50));
    return json({ ok: true, tasks: stmts.length });
  }
  if (p === "/admin/session" && request.method === "POST") {
    // A signed session for a named maintainer, for sets with `readers_prefix`. The page reads it from the URL fragment.
    const b = await body(request);
    if (typeof b.contributor !== "string" || !/^maintainer:[a-z0-9-]{1,40}$/.test(b.contributor)) throw new HttpError(400, "contributor must be maintainer:<name>");
    return json({ session: await signSession(env.SESSION_KEY, b.contributor) });
  }
  if (p === "/admin/dump" && request.method === "GET") {
    // Everything in D1 as JSON lines, one {"table", "row"} per line, for the nightly backup
    // (backup/README.md). The three reads run as one batch, which D1 runs as one transaction,
    // so tasks' counts and the answers come from the same moment and no row is read twice.
    const tables = [["languages", "code"], ["tasks", "id"], ["answers", "task_id, contributor"], ["readings", "task_id, contributor"], ["truths", "task_id"], ["calibration", "task_id"], ["reader_scores", "contributor, task_id"]];
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


// ---------- transcription tasks (transcribe.mjs) ----------

async function isTranscribe(env, l) {
  const row = await env.DB.prepare("SELECT json_extract(config, '$.kind') AS kind FROM languages WHERE code = ?").bind(l).first();
  return !!row && row.kind === "transcribe";
}

async function langConfig(env, l) {
  const row = await env.DB.prepare("SELECT config FROM languages WHERE code = ?").bind(l).first();
  return row ? JSON.parse(row.config) : {};
}

async function scoresOf(env, l, c) {
  const r = await env.DB.prepare("SELECT cer FROM reader_scores WHERE contributor = ? AND lang = ?").bind(c, l).all();
  return r.results.map((x) => x.cer);
}

async function hasCalibration(env, l) {
  const r = await env.DB.prepare("SELECT 1 AS k FROM tasks WHERE lang = ? AND json_extract(body, '$.calibration') = 1 LIMIT 1").bind(l).first();
  return !!r;
}

// Sets with `readers_prefix` (the maintainer's own sets) take readings only from sessions the admin minted.
async function requireReader(env, taskId, c) {
  const t = await env.DB.prepare("SELECT lang, body FROM tasks WHERE id = ?").bind(taskId).first();
  if (!t) return;
  if (JSON.parse(t.body).kind !== "transcribe") throw new HttpError(400, "this task takes a choice, not a reading");
  const cfg = await langConfig(env, t.lang);
  if (cfg.readers_prefix && !c.startsWith(cfg.readers_prefix)) throw new HttpError(403, "this set is read by its maintainer only");
}

async function nextReading(env, l, c, url) {
  const skip = (url.searchParams.get("skip") || "").split(",").filter(Boolean).slice(0, 50);
  const holes = skip.map(() => "?").join(",");
  const notSkipped = skip.length ? `AND t.id NOT IN (${holes})` : "";
  const cfg = await langConfig(env, l);
  const single = cfg.settle === "single";
  const cal = single ? false : await hasCalibration(env, l);
  const scores = await scoresOf(env, l, c);
  const mine = (await env.DB.prepare("SELECT COUNT(*) AS k FROM readings r JOIN tasks t ON t.id = r.task_id WHERE r.contributor = ? AND t.lang = ? AND json_extract(t.body, '$.calibration') IS NULL").bind(c, l).first()).k;
  const open = `t.lang = ? AND NOT EXISTS (SELECT 1 FROM readings r WHERE r.task_id = t.id AND r.contributor = ?) AND NOT EXISTS (SELECT 1 FROM reader_scores s WHERE s.task_id = t.id AND s.contributor = ?) ${notSkipped}`;
  const pick = async (cond, order) =>
    env.DB.prepare(`SELECT t.id, t.body FROM tasks t WHERE ${open} AND ${cond} ORDER BY ${order} LIMIT 1`).bind(l, c, c, ...skip).first();
  let row = null;
  let stage = "read";
  if (wantCalibration(scores.length, mine, cal)) row = await pick("json_extract(t.body, '$.calibration') = 1", "random()");
  if (!row) {
    const ok = qualified(scores, cal, cfg.calibration?.max_cer ?? CAL_MAX_CER);
    if (ok) {
      row = await pick("t.settled = 0 AND t.outcome = 'adjudicate' AND json_extract(t.body, '$.calibration') IS NULL", "random()");
      if (row) stage = "adjudicate";
    }
    if (!row) row = await pick("t.settled = 0 AND t.outcome IS NULL AND json_extract(t.body, '$.calibration') IS NULL", "COALESCE(json_extract(t.body, '$.priority'), 0), t.n DESC, random()");
  }
  const answered = scores.length + mine;
  if (!row) return { task: null, answered };
  const body = JSON.parse(row.body);
  delete body.calibration;
  const task = { id: row.id, ...body, stage };
  if (stage === "adjudicate") {
    const { counted } = await countedReadings(env, row.id, l, cfg);
    const texts = counted.slice(0, 2).map((x) => (x.unreadable ? "" : x.text));
    task.candidates = flip(row.id) ? [texts[1], texts[0]] : texts;
  }
  return { task, answered };
}

async function recordReading(env, taskId, c, rawText, unreadable, at = Date.now()) {
  const t = await env.DB.prepare("SELECT id, lang, settled, body FROM tasks WHERE id = ?").bind(taskId).first();
  if (!t) throw new HttpError(404, "no such task");
  const text = normalizeReading(rawText);
  if (unreadable ? text !== "" : text === "") throw new HttpError(400, unreadable ? "send no text with nothing legible" : "type what is printed, or say nothing is legible");
  if (Array.from(text).length > MAX_TEXT || text.split("\n").length > MAX_LINES) throw new HttpError(400, "text too long");
  const calRow = await env.DB.prepare("SELECT truth FROM calibration WHERE task_id = ?").bind(taskId).first();
  if (calRow) {
    const score = unreadable ? 1 : cer(text, calRow.truth);
    await env.DB.prepare(
      "INSERT INTO reader_scores (contributor, lang, task_id, cer, at) VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT (contributor, task_id) DO UPDATE SET cer = excluded.cer, at = excluded.at",
    ).bind(c, t.lang, taskId, score, at).run();
    // A new score can change whether this person's earlier readings count: recount their tasks, settled or not.
    const mine = (await env.DB.prepare("SELECT r.task_id FROM readings r JOIN tasks t ON t.id = r.task_id WHERE r.contributor = ? AND t.lang = ?").bind(c, t.lang).all()).results;
    for (const m of mine) await resettle(env, m.task_id, t.lang, at);
    return { ok: true };
  }
  if (t.settled) throw new HttpError(409, "this one is already settled");
  await env.DB.prepare(
    "INSERT INTO readings (task_id, contributor, text, unreadable, at) VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT (task_id, contributor) DO UPDATE SET text = excluded.text, unreadable = excluded.unreadable, at = excluded.at",
  ).bind(taskId, c, text, unreadable ? 1 : 0, at).run();
  await resettle(env, taskId, t.lang, at);
  return { ok: true };
}

// The readings that count for a task: only people who qualify, in the order settlement uses.
async function countedReadings(env, taskId, l, cfg) {
  const cal = cfg.settle === "single" ? false : await hasCalibration(env, l);
  const rs = (await env.DB.prepare("SELECT contributor, text, unreadable, at FROM readings WHERE task_id = ?").bind(taskId).all()).results;
  const counted = [];
  for (const r of rs) if (qualified(await scoresOf(env, l, r.contributor), cal, cfg.calibration?.max_cer ?? CAL_MAX_CER)) counted.push(r);
  counted.sort((p, q) => p.at - q.at || (p.contributor < q.contributor ? -1 : 1));
  return { counted, signature: rs.length + ":" + rs.reduce((m, r) => Math.max(m, r.at), 0) };
}

const SIGNATURE_SQL = "(SELECT COUNT(*) || ':' || COALESCE(MAX(at), 0) FROM readings WHERE task_id = ?1)";

// Recounts a task from its stored readings: only people who qualify count. The write is conditional on the
// readings being the ones counted (their number and newest time), so a request that read an older snapshot
// changes nothing and counts again; after a few tries the next reading's recount settles it.
export async function resettle(env, taskId, l, at = Date.now()) {
  const cfg = await langConfig(env, l);
  for (let attempt = 0; attempt < 4; attempt++) {
    const { counted, signature } = await countedReadings(env, taskId, l, cfg);
    const s = settleReadings(counted, cfg.settle === "single" ? "single" : "pair", cfg.agree_cer || 0);
    const stmts = [
      env.DB.prepare(`UPDATE tasks SET n = ?2, settled = ?3, outcome = ?4 WHERE id = ?1 AND ${SIGNATURE_SQL} = ?5`).bind(taskId, s.n, s.settled, s.outcome, signature),
    ];
    if (s.settled) {
      stmts.push(
        env.DB.prepare(`INSERT INTO truths (task_id, text, unreadable, method, at) SELECT ?1, ?2, ?3, ?4, ?5 WHERE ${SIGNATURE_SQL} = ?6 ON CONFLICT (task_id) DO UPDATE SET text = excluded.text, unreadable = excluded.unreadable, method = excluded.method, at = excluded.at`)
          .bind(taskId, s.text, s.unreadable, s.method, at, signature),
      );
    } else stmts.push(env.DB.prepare(`DELETE FROM truths WHERE task_id = ?1 AND ${SIGNATURE_SQL} = ?2`).bind(taskId, signature));
    const res = await env.DB.batch(stmts);
    if (!res[0]?.meta || res[0].meta.changes > 0) return;
  }
}

// CC0 JSON lines: each task with its stage, every reading (readers by a one-way id, never a session id),
// and, once settled, the text and how it settled. promote (scripts/ocr-bench/human_truth.py) re-derives the
// settlement from the readings; the fields here are a convenience.
async function exportReadings(env, l) {
  const cfg = await langConfig(env, l);
  const cal = await hasCalibration(env, l);
  const tasks = (await env.DB.prepare(
    "SELECT t.id, t.set_name, t.body, t.outcome, t.settled, u.text AS truth, u.unreadable AS truth_unreadable, u.method FROM tasks t LEFT JOIN truths u ON u.task_id = t.id WHERE t.lang = ? ORDER BY t.id",
  ).bind(l).all()).results;
  const rs = (await env.DB.prepare("SELECT r.task_id, r.contributor, r.text, r.unreadable, r.at FROM readings r JOIN tasks t ON t.id = r.task_id WHERE t.lang = ? ORDER BY r.task_id, r.at, r.contributor").bind(l).all()).results;
  const sc = (await env.DB.prepare("SELECT contributor, cer FROM reader_scores WHERE lang = ?").bind(l).all()).results;
  const byPerson = new Map();
  for (const x of sc) byPerson.set(x.contributor, [...(byPerson.get(x.contributor) || []), x.cer]);
  const ids = new Map(), ok = new Map();
  for (const r of rs) {
    if (ids.has(r.contributor)) continue;
    ids.set(r.contributor, await readerId(r.contributor));
    const s = byPerson.get(r.contributor) || [];
    ok.set(r.contributor, { qualified: qualified(s, cal, cfg.calibration?.max_cer ?? CAL_MAX_CER), calibration_tasks: s.length, calibration_cer: s.length ? s.reduce((a, v) => a + v, 0) / s.length : null });
  }
  const by = new Map();
  for (const r of rs) by.set(r.task_id, [...(by.get(r.task_id) || []), r]);
  const lines = [];
  for (const t of tasks) {
    const b = JSON.parse(t.body);
    if (b.calibration) continue;
    const mine = by.get(t.id) || [];
    lines.push(JSON.stringify({
      id: t.id, set: t.set_name, kind: "transcribe", page: b.page, source: b.source, label: b.label, line: b.line, unit: b.unit, box: b.box, crop_id: b.crop_id,
      readings: mine.map((r) => ({ reader: ids.get(r.contributor), text: r.unreadable ? null : r.text, unreadable: !!r.unreadable, at: r.at, ...ok.get(r.contributor) })),
      settled: !!t.settled, outcome: t.outcome,
      inter_cer: t.method === "near-agree" || t.outcome === "adjudicate" || t.method === "adjudicated" ? pairCer(mine, ids, ok) : undefined,
      text: t.settled ? (t.truth_unreadable ? null : t.truth) : undefined, method: t.method || undefined,
    }));
  }
  return new Response(lines.join("\n") + "\n", {
    headers: {
      "content-type": "application/x-ndjson; charset=utf-8",
      "content-disposition": `attachment; filename="check-${l}.jsonl"`,
      "cache-control": "public, max-age=60",
      "x-license": "CC0-1.0",
    },
  });
}

// The distance between the first two counted readings of a task (the pair settlement compares).
function pairCer(readings, ids, ok) {
  const two = readings.filter((r) => ok.get(r.contributor)?.qualified).slice(0, 2);
  if (two.length < 2 || two.some((r) => r.unreadable)) return undefined;
  return interCer(two[0].text, two[1].text);
}
