// The letter-check page. "/" lists languages; "/<lang>" shows one word at a time.
const $ = (id) => document.getElementById(id);
const SESSION = "usnm-check-session";
let lang = null, cfg = null, task = null, answered = 0, history = [], skipped = [], busy = false, loaded = false, failed = false;

let memory = {};
function store(k, v) {
  if (v === null) delete memory[k]; else if (v !== undefined) memory[k] = v;
  try { if (v === undefined) return localStorage.getItem(k) || memory[k] || null; if (v === null) localStorage.removeItem(k); else localStorage.setItem(k, v); } catch { /* private mode: memory only */ }
  return v === undefined ? memory[k] || null : null;
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
    status("Starting… if a check box appears below, tick it to begin.");
    // Each attempt gets its own widget, removed afterwards whether it worked or not, so a retry
    // (or a new sign-in after a 401) starts a fresh check instead of waiting on a spent one.
    let widget = null, token;
    try {
      token = await new Promise((resolve, reject) => {
        widget = window.turnstile.render("#turnstile", { sitekey: turnstile_site_key, appearance: "interaction-only", callback: resolve, "error-callback": () => reject(new Error("browser check failed")) });
      });
    } finally {
      if (widget !== null) { try { window.turnstile.remove(widget); } catch { /* already gone */ } }
    }
    const { session: tok } = await getJSON("/api/session", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ turnstile: token }) });
    store(SESSION, tok);
    status("");
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

// Letters as the tasks count them (a_diff, b_diff): a base character with its marks, e.g. פּ or אַ.
const segmenter = typeof Intl !== "undefined" && Intl.Segmenter ? new Intl.Segmenter(undefined, { granularity: "grapheme" }) : null;
function letters(text) {
  if (segmenter) return Array.from(segmenter.segment(text), (s) => s.segment);
  const out = []; // without Intl.Segmenter: marks (dagesh, patah…) stay with the letter before them
  for (const ch of text) { if (out.length && /\p{M}/u.test(ch)) out[out.length - 1] += ch; else out.push(ch); }
  return out;
}

function flip(id) { let h = 0; for (const c of id) h = (h * 31 + c.codePointAt(0)) >>> 0; return (h & 1) === 1; }

// Indexes of letters followed by no space. Where a language counts spellings as the same letter (װ and וו),
// a run of the letters those spellings use (ו, י, װ, ױ, ײ…) is set with no gaps inside it, so equal spellings
// look alike however their letters pair up (יײ and ייי). Highlights keep the original letter indexes.
function tightAfter(ls) {
  const set = new Set();
  for (const [k, v] of Object.entries(cfg.equivalents || {})) { set.add(k); for (const ch of letters(v)) set.add(ch); }
  const out = new Set();
  for (let i = 0; i + 1 < ls.length; i++) if (set.has(ls[i]) && set.has(ls[i + 1])) out.add(i);
  return out;
}

function scriptText(text, diff) {
  const span = document.createElement("span"); span.className = "script"; span.lang = lang;
  if (cfg.dir) span.dir = cfg.dir;
  const marked = new Set(diff), ls = letters(text), tight = tightAfter(ls);
  if (!ls.length) { const g = document.createElement("span"); g.className = "gap"; g.title = "nothing printed here"; span.appendChild(g); return span; }
  ls.forEach((ch, i) => {
    let el;
    if (marked.has(i)) { el = document.createElement("mark"); el.textContent = ch; }
    else if (tight.has(i)) { el = document.createElement("span"); el.textContent = ch; }
    else { span.appendChild(document.createTextNode(ch)); return; }
    if (tight.has(i)) el.className = "tight";
    span.appendChild(el);
  });
  return span;
}

function examples(text, diff) {
  const box = document.createElement("div"); box.className = "examples";
  if (cfg.dir) box.dir = cfg.dir;
  const seen = new Set();
  letters(text).forEach((ch, i) => {
    if (!diff.includes(i) || seen.has(ch)) return; seen.add(ch);
    const ex = document.createElement("span"); ex.className = "ex";
    const l = document.createElement("span"); l.className = "letter"; l.lang = lang; l.textContent = ch; ex.appendChild(l);
    const note = (cfg.notes || {})[ch];
    if (note) { const n = document.createElement("span"); n.className = "note"; n.dir = "auto"; n.textContent = note; ex.appendChild(n); }
    for (const key of ((cfg.examples || {})[ch] || []).slice(0, 3)) {
      const im = new Image(); im.src = "/clips/" + key; im.alt = "printed " + ch; im.loading = "lazy"; ex.appendChild(im);
    }
    box.appendChild(ex);
  });
  if (letters(text).length === 0) {
    const n = document.createElement("span"); n.className = "note"; n.textContent = "Pick this if nothing is printed in the box."; box.appendChild(n);
  }
  return box;
}

function render(prev) {
  const card = $("card"); card.textContent = "";
  $("progress").textContent = answered ? `${answered} answered` : "";
  $("back").disabled = history.length === 0 || busy;
  const live = !!task && !busy;
  $("neither").disabled = $("unsure").disabled = $("notext").disabled = $("skip").disabled = !live;
  if (!task && !loaded) {
    const d = document.createElement("div"); d.className = "done";
    if (busy || !failed) { d.textContent = "Loading the next word…"; card.appendChild(d); return; }
    d.textContent = "The first word didn't load. ";
    const again = document.createElement("button"); again.type = "button"; again.className = "small"; again.id = "retry";
    again.textContent = "Try again"; again.addEventListener("click", () => next(true));
    d.appendChild(again); card.appendChild(d); return;
  }
  if (!task) {
    const d = document.createElement("div"); d.className = "done";
    const s = document.createElement("strong"); s.textContent = "No words left for you";
    d.append(s, document.createTextNode("You've answered every open word, or the rest have enough answers. Thank you. Check back later for more."));
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

// Keyboard users keep their place: when an answer control had focus, the next word's first reading gets it.
function keepFocus(had) {
  if (!had) return;
  const b = $("opt1") || $("retry") || $("back");
  if (b && !b.disabled) b.focus({ preventScroll: true });
}

function controlFocused() { const a = document.activeElement; return !!a && a.tagName === "BUTTON"; }

async function next(had = controlFocused()) {
  busy = true; render();
  try {
    const q = new URLSearchParams({ lang });
    if (skipped.length) q.set("skip", skipped.slice(-50).join(","));
    const r = await authed("/api/next?" + q);
    task = r.task; answered = r.answered; loaded = true; failed = false; status("");
  } catch (e) {
    failed = true;
    status(e.status === 429 ? "Slow down a little, then try again." : "The next word didn't load. " + e.message, true);
  } finally { busy = false; render(); keepFocus(had); }
}

async function choose(choice) {
  if (!task || busy) return;
  const had = controlFocused();
  busy = true; render();
  try {
    await authed("/api/answers", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ task: task.id, choice }) });
    history.push({ task, choice });
    if (history.length > 50) history.shift();
    busy = false;
    await next(had);
  } catch (e) {
    busy = false; render(); keepFocus(had);
    status(e.status === 429 ? "Too fast. Wait a few seconds, then answer again." : "That answer wasn't saved: " + e.message, true);
  }
}

function back() {
  if (!history.length || busy) return;
  const had = controlFocused();
  const h = history.pop(); task = h.task; render(h.choice); keepFocus(had);
  status("Change your answer, or pick the same one to keep it.");
}

function skip() { if (!task || busy) return; skipped.push(task.id); next(controlFocused()); }

async function work(code) {
  lang = code;
  $("work").hidden = false;
  try { cfg = await getJSON("/api/languages/" + code); }
  catch { $("card").textContent = "We don't have this language. "; const a = document.createElement("a"); a.href = "/"; a.textContent = "See the list"; $("card").appendChild(a); return; }
  document.title = `${cfg.name}: USNewsMap letter check`;
  $("intro").textContent = cfg.intro || "";
  $("neither").addEventListener("click", () => choose("neither"));
  $("unsure").addEventListener("click", () => choose("unsure"));
  $("notext").addEventListener("click", () => choose("notext"));
  $("back").addEventListener("click", back);
  $("skip").addEventListener("click", skip);
  document.addEventListener("keydown", (e) => {
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    if (e.key === "1" || e.key === "2") { const b = $("opt" + e.key); if (b) b.click(); }
    else if (e.key === "3") choose("neither");
    else if (e.key === "4") choose("unsure");
    else if (e.key === "5") choose("notext");
    else if (e.key === "s" || e.key === "S") skip();
    else if (e.key === "ArrowLeft" || e.key === "Backspace") back();
  });
  await next();
}

const code = location.pathname.replace(/^\/+|\/+$/g, "");
if (/^[a-z]{2,3}(-[A-Za-z0-9]{2,8})*$/.test(code)) work(code); else home();
