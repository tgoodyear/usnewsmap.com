// The letter-check page. "/" lists languages; "/<lang>" shows one word at a time.
const $ = (id) => document.getElementById(id);
const SESSION = "usnm-check-session";
let lang = null, cfg = null, task = null, answered = 0, history = [], skipped = [], busy = false;

function store(k, v) {
  try { if (v === undefined) return localStorage.getItem(k); if (v === null) localStorage.removeItem(k); else localStorage.setItem(k, v); } catch { /* private mode */ }
  return null;
}

function status(msg, err = false) { const s = $("status"); s.textContent = msg; s.classList.toggle("err", err); }

async function getJSON(path, opts = {}) {
  const r = await fetch(path, opts);
  const body = await r.json().catch(() => ({}));
  if (!r.ok) { const e = new Error(body.error || r.statusText); e.status = r.status; throw e; }
  return body;
}

// A session comes from a Turnstile check, once per browser; it holds a random id and nothing else.
let sessionWaiters = null;
async function session() {
  const s = store(SESSION);
  if (s) return s;
  if (sessionWaiters) return sessionWaiters;
  sessionWaiters = (async () => {
    const { turnstile_site_key } = await getJSON("/api/config");
    while (!window.turnstile) await new Promise((r) => setTimeout(r, 100));
    const token = await new Promise((resolve, reject) => {
      window.turnstile.render("#turnstile", { sitekey: turnstile_site_key, appearance: "interaction-only", callback: resolve, "error-callback": () => reject(new Error("browser check failed")) });
    });
    const { session: tok } = await getJSON("/api/session", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ turnstile: token }) });
    store(SESSION, tok);
    return tok;
  })();
  try { return await sessionWaiters; } finally { sessionWaiters = null; }
}

async function authed(path, opts = {}) {
  for (let attempt = 0; attempt < 2; attempt++) {
    const tok = await session();
    try {
      return await getJSON(path, { ...opts, headers: { ...(opts.headers || {}), authorization: "Bearer " + tok } });
    } catch (e) {
      if (e.status === 401 && attempt === 0) { store(SESSION, null); continue; }
      throw e;
    }
  }
}

// ---------- home ----------

async function home() {
  $("home").hidden = false;
  try {
    const langs = await getJSON("/api/languages");
    const ul = $("langs"); ul.textContent = "";
    for (const l of langs) {
      const li = document.createElement("li"), a = document.createElement("a");
      a.href = "/" + l.code;
      const name = document.createElement("strong"); name.textContent = l.name;
      const meta = document.createElement("div"); meta.className = "meta";
      meta.textContent = `${l.tasks.toLocaleString()} words, ${l.settled.toLocaleString()} settled`;
      a.append(name, meta); li.appendChild(a); ul.appendChild(li);
    }
    if (!langs.length) ul.innerHTML = '<li class="muted">No languages yet.</li>';
  } catch {
    $("langs").innerHTML = '<li class="muted">The list didn\'t load. Reload to try again.</li>';
  }
}

// ---------- one word ----------

function flip(id) { let h = 0; for (const c of id) h = (h * 31 + c.codePointAt(0)) >>> 0; return (h & 1) === 1; }

function scriptText(text, diff) {
  const span = document.createElement("span"); span.className = "script"; span.lang = lang;
  const marked = new Set(diff), letters = Array.from(text);
  if (!letters.length) { const g = document.createElement("span"); g.className = "gap"; g.title = "nothing printed here"; span.appendChild(g); return span; }
  letters.forEach((ch, i) => {
    if (marked.has(i)) { const m = document.createElement("mark"); m.textContent = ch; span.appendChild(m); }
    else span.appendChild(document.createTextNode(ch));
  });
  return span;
}

function examples(text, diff) {
  const box = document.createElement("div"); box.className = "examples";
  const seen = new Set();
  Array.from(text).forEach((ch, i) => {
    if (!diff.includes(i) || seen.has(ch)) return; seen.add(ch);
    const ex = document.createElement("span"); ex.className = "ex";
    const l = document.createElement("span"); l.className = "letter"; l.lang = lang; l.textContent = ch; ex.appendChild(l);
    const note = (cfg.notes || {})[ch];
    if (note) { const n = document.createElement("span"); n.className = "note"; n.textContent = note; ex.appendChild(n); }
    for (const key of ((cfg.examples || {})[ch] || []).slice(0, 3)) {
      const im = new Image(); im.src = "/clips/" + key; im.alt = "printed " + ch; im.loading = "lazy"; ex.appendChild(im);
    }
    box.appendChild(ex);
  });
  if (Array.from(text).length === 0) {
    const n = document.createElement("span"); n.className = "note"; n.textContent = "Pick this if nothing is printed in the box."; box.appendChild(n);
  }
  return box;
}

function render(prev) {
  const card = $("card"); card.textContent = "";
  $("progress").textContent = answered ? `${answered} answered` : "";
  $("back").disabled = history.length === 0 || busy;
  const live = !!task && !busy;
  $("neither").disabled = $("unsure").disabled = $("skip").disabled = !live;
  if (!task) {
    const d = document.createElement("div"); d.className = "done";
    const s = document.createElement("strong"); s.textContent = "No words left for you";
    d.append(s, document.createTextNode("Every open word has your answer, or enough answers already. Thank you. Check back later for more."));
    card.appendChild(d); return;
  }
  const t = task;
  if (t.label) { const top = document.createElement("div"); top.className = "label"; top.textContent = t.label; card.appendChild(top); }
  const lv = document.createElement("div"); lv.className = "lineview";
  const li = new Image(); li.src = "/clips/" + t.line; li.alt = "the printed line"; lv.appendChild(li);
  const hl = document.createElement("div"); hl.className = "hl";
  const [x, y, w, h] = t.box;
  Object.assign(hl.style, { left: 100 * x + "%", top: 100 * y + "%", width: 100 * w + "%", height: 100 * h + "%" });
  lv.appendChild(hl); card.appendChild(lv);
  const wd = document.createElement("div"); wd.className = "word";
  const wi = new Image(); wi.src = "/clips/" + t.word; wi.alt = "the printed word, enlarged"; wd.appendChild(wi);
  card.appendChild(wd);
  const opts = document.createElement("div"); opts.className = "options";
  (flip(t.id) ? ["b", "a"] : ["a", "b"]).forEach((side, k) => {
    const b = document.createElement("button"); b.type = "button"; b.className = "opt"; b.id = "opt" + (k + 1); b.disabled = busy;
    if (prev === side) b.classList.add("chosen");
    const key = document.createElement("span"); key.className = "key"; key.textContent = `Reading ${k + 1}  (${k + 1})`;
    b.append(key, scriptText(t[side], t[side + "_diff"]), examples(t[side], t[side + "_diff"]));
    b.addEventListener("click", () => choose(side));
    opts.appendChild(b);
  });
  card.appendChild(opts);
  if (t.source) {
    const src = document.createElement("a"); src.href = t.source; src.target = "_blank"; src.rel = "noopener"; src.className = "label"; src.textContent = "See the whole page";
    card.appendChild(src);
  }
}

async function next() {
  busy = true; render();
  try {
    const q = new URLSearchParams({ lang });
    if (skipped.length) q.set("skip", skipped.slice(-50).join(","));
    const r = await authed("/api/next?" + q);
    task = r.task; answered = r.answered; status("");
  } catch (e) {
    status(e.status === 429 ? "Slow down a little, then try again." : "The next word didn't load. " + e.message, true);
  } finally { busy = false; render(); }
}

async function choose(choice) {
  if (!task || busy) return;
  busy = true; render();
  try {
    await authed("/api/answers", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ task: task.id, choice }) });
    history.push({ task, choice });
    if (history.length > 50) history.shift();
    busy = false;
    await next();
  } catch (e) {
    busy = false; render();
    status(e.status === 429 ? "That's fast: wait a few seconds, then answer again." : "That answer wasn't saved: " + e.message, true);
  }
}

function back() {
  if (!history.length || busy) return;
  const h = history.pop(); task = h.task; render(h.choice);
  status("Change your answer, or pick the same one to keep it.");
}

function skip() { if (!task || busy) return; skipped.push(task.id); next(); }

async function work(code) {
  lang = code;
  $("work").hidden = false;
  try { cfg = await getJSON("/api/languages/" + code); }
  catch { $("card").textContent = "This language isn't here. "; const a = document.createElement("a"); a.href = "/"; a.textContent = "See the list"; $("card").appendChild(a); return; }
  document.title = `${cfg.name}: USNewsMap letter check`;
  $("intro").textContent = cfg.intro || "";
  $("neither").addEventListener("click", () => choose("neither"));
  $("unsure").addEventListener("click", () => choose("unsure"));
  $("back").addEventListener("click", back);
  $("skip").addEventListener("click", skip);
  document.addEventListener("keydown", (e) => {
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    if (e.key === "1" || e.key === "2") { const b = $("opt" + e.key); if (b) b.click(); }
    else if (e.key === "3") choose("neither");
    else if (e.key === "4") choose("unsure");
    else if (e.key === "s" || e.key === "S") skip();
    else if (e.key === "ArrowLeft" || e.key === "Backspace") back();
  });
  await next();
}

const code = location.pathname.replace(/^\/+|\/+$/g, "");
if (/^[a-z]{2,3}(-[A-Za-z0-9]{2,8})*$/.test(code)) work(code); else home();
