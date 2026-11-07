// check.usnewsmap.com: people compare a printed word with two OCR readings and say which
// matches (#1620). The page is static (public/); this Worker answers /api/*, /clips/* and
// /admin/*. README.md describes the data, the settling rule and how tasks are loaded.

const CHOICES = new Set(["a", "b", "neither", "unsure"]);
const SESSION_DAYS = 30;
const SETTLE_MIN = 3; // answers before a word can settle
const DISPUTE_AT = 7; // answers after which a word that never agreed is closed as disputed
const enc = new TextEncoder();

export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);
    try {
      if (url.pathname.startsWith("/api/")) return await api(request, env, url);
      if (url.pathname.startsWith("/clips/")) return await clip(request, env, url);
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

// counts: {a, b, neither, unsure}. A word settles once at least SETTLE_MIN people answered and
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
  if (p === "/api/answers" && request.method === "POST") {
    const c = await contributorOf(request, env, { limited: true });
    const b = await body(request);
    if (typeof b.task !== "string" || b.task.length > 300 || !CHOICES.has(b.choice)) throw new HttpError(400, "task and choice required");
    const t = await env.DB.prepare("SELECT id FROM tasks WHERE id = ?").bind(b.task).first();
    if (!t) throw new HttpError(404, "no such task");
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
      `SELECT t.id, t.set_name, t.body, t.outcome, t.settled,
         SUM(a.choice = 'a') AS a, SUM(a.choice = 'b') AS b, SUM(a.choice = 'neither') AS neither, SUM(a.choice = 'unsure') AS unsure
       FROM tasks t LEFT JOIN answers a ON a.task_id = t.id WHERE t.lang = ? GROUP BY t.id ORDER BY t.id`,
    )
      .bind(l)
      .all();
    const lines = rows.results.map((r) => {
      const t = JSON.parse(r.body);
      return JSON.stringify({
        id: r.id, set: r.set_name, page: t.page, source: t.source, a: t.a, b: t.b, a_is: t.a_is, b_is: t.b_is,
        counts: { a: r.a || 0, b: r.b || 0, neither: r.neither || 0, unsure: r.unsure || 0 },
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
export const SETTLE_SQL = `
WITH c AS (
  SELECT COUNT(*) AS n,
    COALESCE(SUM(choice = 'a'), 0) AS a, COALESCE(SUM(choice = 'b'), 0) AS b,
    COALESCE(SUM(choice = 'neither'), 0) AS ne, COALESCE(SUM(choice = 'unsure'), 0) AS u
  FROM answers WHERE task_id = ?1
), t AS (
  SELECT n, MAX(a, b, ne, u) AS top,
    CASE WHEN a >= b AND a >= ne AND a >= u THEN 'a' WHEN b >= ne AND b >= u THEN 'b' WHEN ne >= u THEN 'neither' ELSE 'unsure' END AS pick
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

async function clip(request, env, url) {
  if (request.method !== "GET" && request.method !== "HEAD") throw new HttpError(405, "method not allowed");
  const key = decodeURIComponent(url.pathname.slice("/clips/".length));
  if (!/^[A-Za-z0-9._\-/]{1,300}$/.test(key) || key.includes("..")) throw new HttpError(404, "not found");
  const obj = await env.CLIPS.get(key);
  if (!obj) throw new HttpError(404, "not found");
  return new Response(obj.body, {
    headers: {
      "content-type": obj.httpMetadata?.contentType || "image/jpeg",
      "cache-control": "public, max-age=31536000, immutable",
      etag: obj.httpEtag,
    },
  });
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
  if (p === "/admin/dump" && request.method === "GET") {
    // Everything in D1 as JSON lines, one {"table", "row"} per line, for the nightly backup
    // (backup/README.md). Read in pages so a large answers table stays inside D1's result limits.
    const tables = [["languages", "code"], ["tasks", "id"], ["answers", "task_id, contributor"]];
    const out = [];
    for (const [table, order] of tables) {
      for (let offset = 0; ; offset += 5000) {
        const rows = await env.DB.prepare(`SELECT * FROM ${table} ORDER BY ${order} LIMIT 5000 OFFSET ?`).bind(offset).all();
        for (const row of rows.results) out.push(JSON.stringify({ table, row }));
        if (rows.results.length < 5000) break;
      }
    }
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
