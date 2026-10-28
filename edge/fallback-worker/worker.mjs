// The fallback Worker (ADR-0027 decision 10, #1098). During Phase 3 it sits on the
// usnewsmap.com, www and api routes in front of the Cloudflare Tunnel. Every request goes
// to the tunnel. Only when the tunnel cannot answer does it try the same request once at
// the Container App's default azurecontainerapps.io name:
//
//   - the tunnel answers 502, or 530 (Cloudflare's error 1033, a tunnel with no
//     connectors, arrives as a 530), or
//   - the fetch to the tunnel throws, unless it was aborted.
//
// It never retries anything else. A timeout (504, 524, a TimeoutError) could mean the
// origin did the work, and a repeated search would be logged twice. The API's own
// problem+json 503s (a searcher restart, a failed readiness check) are answers, not
// outages, so they pass through. There is at most one retry: whatever Azure answers is
// returned, so a failure there cannot loop.
//
// The Worker logs nothing. URLs carry the search words in `q`, so no console call may be
// added here (worker.test.mjs checks), and Workers Logs stay off in wrangler.jsonc.
//
// Configuration, all in `env`:
//   AZURE_FALLBACK_ORIGIN  https://<name>.azurecontainerapps.io, the app's default name.
//                          Anything else turns the fallback off, so a typo cannot send
//                          visitors' requests to another host.
//   ADD_DNT                "true" on the drill Worker only: sends `DNT: 1` to both origins,
//                          so searches made during a drill stay out of the search log
//                          (the API's admit() excludes DNT, crates/usnm-api/src/searchlog.rs).
//   ERROR_LAYER            "true" only after the Phase 4 gate, or on an isolated drill.
//   ALLOWED_ORIGINS        Exact comma-separated USNM_ALLOWED_ORIGINS from the API.
//   FALLBACK_METRICS        Analytics Engine binding: fixed outcome/scope/count/status,
//                          never URLs, search words, visitor headers or addresses.

// 502: the tunnel's connector could not reach Caddy or the API. 530: the tunnel has no
// connector. Phase 2's outage drills (#1101) confirm what a Worker receives in each case;
// change this set only with their results.
const RETRY_STATUSES = new Set([502, 530]);

// The search log admits a search only when Origin or Sec-Fetch-Site says it came from the
// site itself (admit() in crates/usnm-api/src/searchlog.rs), so a search answered by Azure
// reaches the log only if these arrive as the browser sent them. The copy of every header
// carries them already; setting them again, by name, keeps the rule in view and holds if
// the copy is ever narrowed to a list.
const FORWARDED_AS_RECEIVED = ["origin", "sec-fetch-site"];

// The only hosts the fallback may send a request to: a Container Apps default name.
const AZURE_ORIGIN_HOST = /^([a-z0-9]([a-z0-9-]*[a-z0-9])?\.)+azurecontainerapps\.io$/;

export default {
  async fetch(request, env = {}) {
    // Cloned before the first fetch, which uses the body up: a POST (the page-view beacon)
    // has to reach Azure whole when the tunnel fails.
    const replay = request.clone();
    let tunnel;
    try {
      tunnel = await fetch(toTunnel(request, env));
    } catch (error) {
      if (aborted(error, request)) throw error;
      if (errorLayer(env)) return degraded(replay, env);
      return fallBack(replay, env, null, error);
    }
    if (!RETRY_STATUSES.has(tunnel.status)) {
      return errorLayer(env) ? rememberShell(replay, tunnel) : tunnel;
    }
    if (errorLayer(env)) {
      let cleanupFailed = false;
      try {
        await tunnel.body?.cancel();
      } catch {
        cleanupFailed = true;
      }
      const answer = await degraded(replay, env);
      return cleanupFailed ? notified(answer, "X-USNM-Error-Layer-Cleanup", "cancel-failed") : answer;
    }
    return fallBack(replay, env, tunnel, null);
  },
};

// Azure's answer, whatever it is. If Azure cannot answer at all, the tunnel's own answer
// when it gave one (the visitor sees what they would without a Worker), else Azure's error.
async function fallBack(replay, env, tunnel, tunnelError) {
  const target = azureUrl(replay.url, env.AZURE_FALLBACK_ORIGIN);
  if (target === null) {
    if (tunnel !== null) return tunnel;
    throw tunnelError;
  }
  let answer;
  try {
    answer = await fetch(toAzure(replay, target, env));
  } catch (error) {
    if (tunnel !== null) return tunnel;
    throw error;
  }
  if (tunnel !== null) await tunnel.body?.cancel().catch(() => {});
  return counted(answer, env, "azure");
}

function notified(answer, header, value) {
  const copy = new Response(answer.body, answer);
  copy.headers.set(header, value);
  return copy;
}

function counted(answer, env, outcome) {
  if (!env.FALLBACK_METRICS) {
    return notified(answer, "X-USNM-Fallback-Metric", "unavailable");
  }
  try {
    const scope = addDnt(env) ? "drill" : "production";
    env.FALLBACK_METRICS.writeDataPoint({
      blobs: [outcome, scope],
      doubles: [1, answer.status],
      indexes: [scope],
    });
  } catch {
    return notified(answer, "X-USNM-Fallback-Metric", "write-failed");
  }
  return answer;
}

function errorLayer(env) {
  return env.ERROR_LAYER === "true" || env.ERROR_LAYER === "1";
}

function shellKey(request) {
  return new Request(`${new URL(request.url).origin}/__usnm_fallback/shell-v1`);
}

async function rememberShell(request, answer) {
  const url = new URL(request.url);
  if (
    request.method !== "GET" || url.pathname !== "/" || url.search !== "" ||
    request.headers.has("authorization") || request.headers.has("cookie") ||
    answer.status !== 200 || answer.headers.has("set-cookie") ||
    !answer.headers.get("content-type")?.toLowerCase().startsWith("text/html") ||
    /\b(private|no-store)\b/i.test(answer.headers.get("cache-control") ?? "")
  ) return answer;
  const stored = new Response(answer.clone().body, answer);
  stored.headers.set("Cache-Control", "public, max-age=86400");
  try {
    await caches.default.put(shellKey(request), stored);
  } catch {
    return notified(answer, "X-USNM-Error-Layer", "cache-write-failed");
  }
  return answer;
}

async function degraded(request, env) {
  const path = new URL(request.url).pathname.replace(/\/+$/, "") || "/";
  const api = path === "/v1" || path.startsWith("/v1/") ||
    path === "/api/v1" || path.startsWith("/api/v1/");
  const html = !api && (request.method === "GET" || request.method === "HEAD") &&
    (path === "/" || ["/index.html", "/datasets", "/status", "/privacy", "/credits"].includes(path) ||
      path.startsWith("/search/")) &&
    request.headers.get("accept")?.toLowerCase().includes("text/html");
  if (html) {
    let shell;
    try {
      shell = await caches.default.match(shellKey(request));
    } catch {
      return counted(problem(request, "cache-read-failed", env), env, "degraded");
    }
    if (shell) {
      const headers = new Headers(shell.headers);
      headers.set("Cache-Control", "no-store");
      headers.set("Retry-After", "30");
      headers.set("X-USNM-Error-Layer", "cached-shell");
      for (const header of ["etag", "last-modified", "content-length"]) headers.delete(header);
      const answer = new Response(request.method === "HEAD" ? null : shell.body, { status: 503, headers });
      if (request.method === "HEAD") return counted(answer, env, "degraded");
      const marked = new HTMLRewriter().on("body", {
        element(element) {
          element.prepend(
            '<div role="status">Search is temporarily unavailable. Please try again later.</div>',
            { html: true },
          );
        },
      }).transform(answer);
      return counted(marked, env, "degraded");
    }
  }
  return counted(problem(request, "unavailable", env), env, "degraded");
}

function problem(request, reason, env) {
  const headers = new Headers({
    "Content-Type": "application/problem+json",
    "Cache-Control": "no-store",
    "Retry-After": "30",
    "X-USNM-Error-Layer": reason,
    "Vary": "Origin",
  });
  const origin = request.headers.get("origin");
  const allowed = typeof env.ALLOWED_ORIGINS === "string" ? env.ALLOWED_ORIGINS.split(",").map((s) => s.trim()) : [];
  if (origin && allowed.includes(origin)) headers.set("Access-Control-Allow-Origin", origin);
  return new Response(request.method === "HEAD" ? null : JSON.stringify({
    type: "about:blank",
    title: "Service Unavailable",
    status: 503,
    detail: "The origin is temporarily unavailable. Please retry later.",
  }), {
    status: 503,
    headers,
  });
}

function toTunnel(request, env) {
  if (!addDnt(env)) return request;
  const headers = new Headers(request.headers);
  headers.set("DNT", "1");
  return new Request(request, { headers });
}

function toAzure(replay, target, env) {
  const headers = new Headers(replay.headers);
  // The Host header names the site. Azure is reached by its own name, so the runtime sets it.
  headers.delete("host");
  for (const name of FORWARDED_AS_RECEIVED) {
    const value = replay.headers.get(name);
    if (value !== null) headers.set(name, value);
  }
  if (addDnt(env)) headers.set("DNT", "1");
  // Redirects belong to the visitor's browser, not to the Worker. The client IP headers
  // (CF-Connecting-IP, X-Forwarded-For) are neither added nor removed here: the Worker
  // sets no header of its own that the API could be asked to trust.
  return new Request(target, {
    method: replay.method,
    headers,
    body: replay.body,
    redirect: "manual",
    duplex: "half",
  });
}

// The request's own path and query at the Azure name, or null when the name is not one the
// fallback may use. The path is assigned, never parsed against the base: `//host/x` would
// otherwise name another host.
function azureUrl(requestUrl, origin) {
  if (typeof origin !== "string") return null;
  let base;
  try {
    base = new URL(origin);
  } catch {
    return null;
  }
  const plain =
    base.protocol === "https:" &&
    base.port === "" &&
    base.username === "" &&
    base.password === "" &&
    base.pathname === "/" &&
    base.search === "" &&
    base.hash === "" &&
    AZURE_ORIGIN_HOST.test(base.hostname);
  if (!plain) return null;
  const incoming = new URL(requestUrl);
  const target = new URL(base);
  target.pathname = incoming.pathname;
  target.search = incoming.search;
  return target;
}

function addDnt(env) {
  return env.ADD_DNT === "true" || env.ADD_DNT === "1";
}

// A visitor who goes away, or a deadline that passes, is not an outage of the tunnel.
function aborted(error, request) {
  return error?.name === "AbortError" || error?.name === "TimeoutError" || request.signal?.aborted === true;
}
