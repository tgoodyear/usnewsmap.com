// The letter-check page. "/" lists languages; "/<lang>" shows one word at a time.
const $ = (id) => document.getElementById(id);
const SESSION = "usnm-check-session";
const NAMES = "usnm-check-names"; // "1" when the visitor turned letter names on in this browser
const KEYOPEN = "usnm-check-key"; // "1" or "0": the visitor opened or closed the letter key in this browser
let lineShown = true; // column gutter tasks: whether the line is drawn on the crop shown; each new crop starts with it drawn
let lang = null, cfg = null, task = null, answered = 0, history = [], skipped = [], busy = false, loaded = false, failed = false, fontFailed = false, showNames = false, shownPrev;
// After "Neither matches", the word stays on screen with an optional box for what the print says (suggestBox). suggestFor is the
// word it is open on; suggestDraft keeps what was typed through a re-render. Worker: POST /api/suggestions, max SUGGEST_MAX.
// suggestOpening is the object made each time the box opens: a save belongs to the opening it started from, so a save that
// finishes after the box was closed and reopened (Send, Skip, Back, Neither) neither closes the new box nor blocks its Enter key.
// The characters offered under the box by default (suggestBox): the single-character fractions newspapers print in prices and
// addresses. The tools compare them with 1/2 typed out as equal (tools/build_tasks.py, fold_fractions).
const INSERT_CHARS = ["½", "¼", "¾", "⅓", "⅔", "⅛", "⅜", "⅝", "⅞"];
const CHARSOPEN = "usnm-check-chars"; // "1" when the visitor opened the character buttons under the suggestion box
let suggestFor = null, suggestDraft = "", suggestOpening = null, suggestSending = null;
// On a slow connection an answer is saved in the background and the next word, fetched while this one was shown, appears at once.
// `pending` holds answers not yet saved, oldest first; `ahead` holds the next words fetched for the word now shown; `stale` is set
// when the Worker says this page is out of date, and stops answering until a reload.
let pending = [], sending = false, inflight = null, ahead = null, stale = false, slowTimer = null, wake = null, drained = [], refused = 0;
const counted = new Set(); // words answered on this page, so a changed answer isn't counted twice
// Sent with every answer. The Worker refuses answers to a word marked confusable_display from a page older than this: an
// already-open tab has the old stylesheet and font, and would send answers about letters it draws alike (worker.mjs, DISPLAY_REVISION).
// Revision 2 draws a gutter task's leaning line (line.x1); the Worker refuses answers to such a task from an older page.
const DISPLAY = 2;
const SUGGEST_MAX = 200;

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
let sessionWaiters = null, checking = false; // checking: the browser check is showing, so its instruction stays in the status line
async function session() {
  const s = store(SESSION);
  if (s) return s;
  if (sessionWaiters) return sessionWaiters;
  sessionWaiters = (async () => {
    const { turnstile_site_key } = await getJSON("/api/config");
    // Wait for Turnstile's own object: until its script has run, window.turnstile can be something else (an element whose id is
    // "turnstile" is one), and a first visit whose script loads after this one failed with "render is not a function".
    // A script that never loads (blocked, offline) ends the wait with a message instead of "Loading" for ever.
    for (const until = Date.now() + 30000; !(window.turnstile && typeof window.turnstile.render === "function"); ) {
      if (Date.now() > until) throw new Error("the browser check didn't load; check your connection or ad blocker, then reload");
      await new Promise((r) => setTimeout(r, 100));
    }
    checking = true;
    status("Running a one-time browser check… if a check box appears above the word, tick it to begin.");
    // Each attempt gets its own widget, removed afterwards whether it worked or not, so a retry
    // (or a new sign-in after a 401) starts a fresh check instead of waiting on a spent one.
    let widget = null, token;
    try {
      token = await new Promise((resolve, reject) => {
        widget = window.turnstile.render("#turnstile-box", { sitekey: turnstile_site_key, appearance: "interaction-only", callback: resolve, "error-callback": () => reject(new Error("browser check failed")) });
      });
    } finally {
      checking = false;
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
      // Answers save while the next words load, so a 401 can come back after another request has already renewed the
      // session: only a session still stored as the one refused is thrown away; otherwise the retry uses the new one.
      if (e.status === 401 && attempt === 0) { if (store(SESSION) === tok) store(SESSION, null); continue; }
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
// One-for-one equivalents (Serbian's Latin look-alikes, C and С) never pair up differently, so they stay spaced.
function tightAfter(ls) {
  const set = new Set();
  for (const [k, v] of Object.entries(cfg.equivalents || {})) {
    if (letters(k).length === letters(v).length) continue;
    for (const ch of letters(k).concat(letters(v))) set.add(ch);
  }
  const out = new Set();
  for (let i = 0; i + 1 < ls.length; i++) if (set.has(ls[i]) && set.has(ls[i + 1])) out.add(i);
  return out;
}

// The name printed under a letter when the option is on: the language's `labels` entry (the syllable for a Cherokee letter,
// the Latin equivalent of a Serbian Cyrillic one). A language with none has no letter names, and the option is not offered.
const nameOf = (ch) => (cfg.labels || {})[ch];
const hasNames = () => !!cfg && !!cfg.labels && Object.keys(cfg.labels).length > 0;

// With `named`, a letter that has a name is set in a cell: the letter, and the name centred under it in small type. The cell
// is as wide as the wider of the two, so no name can touch the next one, and the cells are spaced evenly; the letters
// do not keep the advances they have without names. A letter with no name (a space, a digit, a bracket) stays plain text.
// The highlight is still the letter's own box, with no padding, so it sits on the letter alone.
function scriptText(text, diff, named = false) {
  const span = document.createElement("span"); span.className = "script"; span.lang = lang;
  if (cfg.dir) span.dir = cfg.dir;
  const marked = new Set(diff), ls = letters(text), tight = tightAfter(ls);
  if (!ls.length) { const g = document.createElement("span"); g.className = "gap"; g.title = "nothing printed here"; span.appendChild(g); return span; }
  if (named) span.classList.add("named");
  ls.forEach((ch, i) => {
    const name = named ? nameOf(ch) : undefined;
    if (name !== undefined) {
      const cell = document.createElement("span"); cell.className = "cell";
      const g = document.createElement(marked.has(i) ? "mark" : "span"); g.className = "glyph"; g.textContent = ch;
      const n = document.createElement("span"); n.className = "n"; n.dir = "ltr"; n.textContent = name;
      cell.append(g, n); span.appendChild(cell); return;
    }
    let el;
    if (marked.has(i)) { el = document.createElement("mark"); el.textContent = ch; }
    else if (tight.has(i)) { el = document.createElement("span"); el.textContent = ch; }
    else { span.appendChild(document.createTextNode(ch)); return; }
    if (tight.has(i)) el.classList.add("tight");
    if (marked.has(i) && i > 0 && tight.has(i - 1)) el.classList.add("tight-before");
    span.appendChild(el);
  });
  return span;
}

// The names shown under a reading for pairs the task build flagged as look-alikes in the page's font (lookalikes:
// [{a, a_label, b, b_label}], letter indexes as a_diff and b_diff). A Map from letter index to name. A flagged letter
// is shown even where the highlight doesn't mark it (a transposition). The name is, in order:
//  - the letter's entry in the language's `labels`, e.g. the Latin equivalent of a Serbian Cyrillic letter;
//  - "Latin <letter>" for a Latin letter in a language that has `labels` (a Latin look-alike in a Cyrillic reading);
//  - the name the task stored: the syllable for Cherokee, else the Unicode character name.
// When one reading of the pair is a Latin letter and the other is Cyrillic, the labelled letter also says its script
// ("Cyrillic ć" against "Latin h"); a Cherokee letter keeps its plain name, so two labels never read as the same letter.
const LATIN = /^\p{Script=Latin}/u;
const CYRILLIC = /^\p{Script=Cyrillic}/u;
function lookalikeLabels(t, side) {
  const labels = new Map(), mine = letters(t[side] || ""), other = side === "a" ? "b" : "a", theirs = letters(t[other] || ""), names = cfg.labels;
  for (const p of t.lookalikes || []) {
    if (typeof p[side + "_label"] !== "string" || !(p[side] < mine.length)) continue;
    let label = p[side + "_label"];
    if (names) {
      const ch = mine[p[side]], named = names[ch], their = theirs[p[other]];
      if (named !== undefined) label = (their !== undefined && LATIN.test(their) && CYRILLIC.test(ch) ? "Cyrillic " : "") + named;
      else if (LATIN.test(ch)) label = "Latin " + ch;
    }
    labels.set(p[side], label);
  }
  return labels;
}

// The marks each reading has on a letter where the two readings are the same base letter and differ only in marks
// (mark_only: [{a, a_marks, b, b_marks, base}], the mark characters as strings, "" for none). A Map from letter index to
// the text shown: the names of the marks (the language's `marks`; a mark with no name is "another mark", never the Unicode
// name), or "no mark". Tasks built before the field existed have none, and show nothing here.
function markLabels(t, side) {
  const labels = new Map(), n = letters(t[side]).length, names = cfg.marks || {};
  for (const p of t.mark_only || []) {
    const marks = p[side + "_marks"];
    if (typeof marks !== "string" || !(p[side] < n)) continue;
    const shown = [...new Set([...marks].map((m) => names[m] || "another mark"))];
    labels.set(p[side], shown.length ? shown.join(" + ") : "no mark");
  }
  return labels;
}

// With `named` (the letter names are showing), a look-alike label that is the same text as the name under the letter is not
// printed again; the picture row for the letter stays.
function examples(text, diff, labels = new Map(), marks = new Map(), named = false) {
  const box = document.createElement("div"); box.className = "examples";
  if (cfg.dir) box.dir = cfg.dir;
  const seen = new Set();
  letters(text).forEach((ch, i) => {
    const name = labels.get(i), mark = marks.get(i), key = ch + "|" + (name || "") + "|" + (mark || "");
    if ((!diff.includes(i) && name === undefined && mark === undefined) || seen.has(key)) return; seen.add(key);
    const ex = document.createElement("span"); ex.className = "ex";
    const l = document.createElement("span"); l.className = "letter"; l.lang = lang; l.textContent = ch; ex.appendChild(l);
    if (name !== undefined && !(named && nameOf(ch) === name)) {
      const n = document.createElement("span"); n.className = "name"; n.dir = "ltr"; n.textContent = name; ex.appendChild(n);
    }
    if (mark !== undefined) {
      // The marks this reading has on the letter, as text, and the letter at about twice the chip's size so a small mark shows.
      // The inset is a picture of the letter the page already names, so screen readers skip it.
      const n = document.createElement("span"); n.className = "name"; n.dir = "ltr"; n.textContent = mark; ex.appendChild(n);
      const big = document.createElement("span"); big.className = "inset"; big.lang = lang; big.setAttribute("aria-hidden", "true");
      if (cfg.dir) big.dir = cfg.dir;
      big.textContent = ch; ex.appendChild(big);
    }
    const note = (cfg.notes || {})[ch];
    if (note) { const n = document.createElement("span"); n.className = "note"; n.dir = "auto"; n.textContent = note; ex.appendChild(n); }
    for (const key of ((cfg.examples || {})[ch] || []).slice(0, 3)) {
      const im = new Image(); im.src = "/clips/" + key; im.alt = "printed " + ch; im.loading = "lazy"; ex.appendChild(im);
    }
    box.appendChild(ex);
  });
  if (letters(text).length === 0) {
    const n = document.createElement("span"); n.className = "note"; n.dir = "ltr"; n.textContent = "Pick this if nothing is printed in the box."; box.appendChild(n);
  }
  return box;
}

// A Cherokee reading is only answerable once the served font has loaded: otherwise a visitor's installed font may draw
// letters alike (#1939). Resolves false when the font failed to load; true when it has, or the task has no Cherokee letters.
const CHEROKEE = /[\u13A0-\u13FF\uAB70-\uABBF]/;
// The fonts the page serves, by the letters they cover (style.css, @font-face). The family is the one the page declares.
const BUNDLED = [[CHEROKEE, "Noto Sans Cherokee"]];
const bundledFamily = (t) => { const hit = t && BUNDLED.find(([re]) => re.test(t.a + t.b)); return hit ? hit[1] : null; };
async function fontReady(t) {
  const family = bundledFamily(t);
  if (!family || !document.fonts || !document.fonts.load) return true;
  try {
    const faces = await document.fonts.load(`32px "${family}"`, t.a + t.b);
    return faces.length > 0 && faces.every((f) => f.status === "loaded");
  } catch { return false; }
}

// Letter names are showing: the visitor turned them on and the language has names to show.
const namesOn = () => showNames && hasNames();

// What the page reports with an answer about letter names (worker.mjs, NAMES_STATES): `shown` when the option was on and at
// least one letter of either reading had a name under it, else `not_shown`, which includes every language the option is not
// offered for. Nothing else about the visitor is sent.
function namesState(t) {
  if (!namesOn() || !t) return "not_shown";
  return [t.a, t.b].some((text) => letters(text || "").some((ch) => nameOf(ch) !== undefined)) ? "shown" : "not_shown"; // each reading on its own, as it is drawn
}

// What the page reports with an answer about the bundled font (worker.mjs, FONT_STATES): the word's bundled font has
// loaded or not, no bundled font applies to its letters, or the browser can't say. Nothing else about the visitor's fonts
// is read or sent.
function fontState(t) {
  const family = bundledFamily(t);
  if (!family) return "none";
  if (!document.fonts || !document.fonts.check) return "unknown";
  try { return document.fonts.check(`32px "${family}"`, t.a + t.b) ? "loaded" : "not_loaded"; } catch { return "unknown"; }
}

// Column gutter tasks (README.md, "Column gutter judgments"): one crop with a line drawn over it, and the question whether the line
// runs down a gap between columns. Yes is stored as `a`, No as `b`; neither and not text don't apply. The line is a CSS overlay at
// the task's fractions of the crop, so the picture itself is never changed and the line can be hidden to see the print under it.
// A line with `x1` (its x at y1; x is its x at y0) leans to follow a tilted page: it is drawn as an SVG line over the whole crop,
// in percentages of it, so it keeps its ends on the same print as the picture scales. A line without x1 is the vertical div.
const isGutter = (t) => !!t && t.kind === "gutter";
const GUTTER_ANSWERS = [["a", "Yes"], ["b", "No"]];
// The help under the buttons for Bad crop (README.md, "Bad crop"). A gutter crop holds many printed lines by design, and a line that
// crosses print is a No, so its task says what a broken gutter crop is instead.
const BADCROP_HELP = "Pick Bad crop if the box shows more than one line, cuts off the word, or marks the wrong thing.";
const GUTTER_BADCROP_HELP = "Pick Bad crop if the picture is broken or isn't part of a newspaper page, or the line is missing or runs off the picture. A line that crosses print is a No, not a bad crop.";

// Cover tasks (README.md, "Cover tasks"): a block of print, where it is on the page, and one machine reading of that place, and
// the question whether all of the block's text is in the reading. Yes is `a`, No (some is missing) is `b`, Not text is `notext`
// (the block holds no text); Neither doesn't apply. Wrong letters don't count, only missing lines or passages.
const isCover = (t) => !!t && t.kind === "cover";
const COVER_ANSWERS = [["a", "Yes, it's all there"], ["b", "No, some is missing"]];
const COVER_BADCROP_HELP = "Pick Bad crop if the picture is broken, or the box cuts through the middle of lines so you can't say what is in it.";

function renderCover(card, t, prev) {
  const q = document.createElement("p"); q.className = "question"; q.textContent = cfg.question || ""; card.appendChild(q);
  if (t.label) { const top = document.createElement("div"); top.className = "label"; top.textContent = t.label; card.appendChild(top); }
  const pv = document.createElement("div"); pv.className = "lineview pageview";
  const pi = new Image(); pi.src = "/clips/" + t.pageview; pi.alt = "the whole page, with the block marked"; pv.appendChild(pi);
  const hl = document.createElement("div"); hl.className = "hl";
  const [x, y, w, h] = t.box;
  Object.assign(hl.style, { left: 100 * x + "%", top: 100 * y + "%", width: 100 * w + "%", height: 100 * h + "%" });
  pv.appendChild(hl); card.appendChild(pv);
  const bk = document.createElement("div"); bk.className = "word block";
  const bi = new Image(); bi.src = "/clips/" + t.block; bi.alt = "the printed block, enlarged"; bk.appendChild(bi);
  card.appendChild(bk);
  const rd = document.createElement("div"); rd.className = "reading";
  const rk = document.createElement("div"); rk.className = "key"; rk.textContent = "The machine reading of this place";
  const body = document.createElement("div"); body.className = "reading-text"; body.lang = "en";
  if (t.text) body.textContent = t.text; else { body.classList.add("empty"); body.textContent = "(this reading has no text here)"; }
  rd.append(rk, body); card.appendChild(rd);
  const opts = document.createElement("div"); opts.className = "options gutteropts";
  COVER_ANSWERS.forEach(([choice, text], k) => {
    const b = document.createElement("button"); b.type = "button"; b.className = "opt"; b.id = "opt" + (k + 1); b.disabled = busy || stale;
    if (prev === choice) b.classList.add("chosen");
    const kk = document.createElement("span"); kk.className = "key"; kk.textContent = `${k + 1}`;
    const big = document.createElement("span"); big.className = "yn"; big.textContent = text;
    b.append(kk, big);
    b.addEventListener("click", () => choose(choice));
    opts.appendChild(b);
  });
  card.appendChild(opts);
}

function renderGutter(card, t, prev) {
  const q = document.createElement("p"); q.className = "question"; q.textContent = cfg.question || ""; card.appendChild(q);
  const view = document.createElement("div"); view.className = "gutterview";
  // Focusable so keyboard-only volunteers can scroll a tall crop with the arrow keys.
  view.tabIndex = 0; view.setAttribute("role", "region"); view.setAttribute("aria-label", "Page crop; scroll to see the whole line");
  const inner = document.createElement("div"); inner.className = "gutterinner";
  const { x, y0, y1 } = t.line;
  const x1 = typeof t.line.x1 === "number" ? t.line.x1 : x;
  const leans = x1 !== x;
  const im = new Image(); im.src = "/clips/" + t.crop;
  im.alt = leans ? "a stretch of newspaper page with a slanting line drawn down it" : "a stretch of newspaper page with a vertical line drawn on it";
  inner.appendChild(im);
  const ln = document.createElement("div"); ln.className = "gline"; ln.id = "gline"; ln.hidden = !lineShown;
  if (leans) {
    ln.classList.add("lean");
    const NS = "http://www.w3.org/2000/svg";
    const svg = document.createElementNS(NS, "svg"); svg.setAttribute("aria-hidden", "true"); svg.setAttribute("focusable", "false");
    for (const cls of ["halo", "stroke"]) {
      const l = document.createElementNS(NS, "line"); l.setAttribute("class", cls);
      for (const [a, v] of [["x1", x], ["y1", y0], ["x2", x1], ["y2", y1]]) l.setAttribute(a, 100 * v + "%");
      svg.appendChild(l);
    }
    ln.appendChild(svg);
  } else {
    Object.assign(ln.style, { left: 100 * x + "%", top: 100 * y0 + "%", height: 100 * (y1 - y0) + "%" });
  }
  inner.appendChild(ln); view.appendChild(inner); card.appendChild(view);
  const bar = document.createElement("div"); bar.className = "gutterbar";
  const key = document.createElement("span"); key.className = "gkey";
  const sw = document.createElement("span"); sw.className = "swatch"; sw.setAttribute("aria-hidden", "true");
  key.append(sw, document.createTextNode(" The line to judge"));
  const tog = document.createElement("button"); tog.type = "button"; tog.className = "small"; tog.id = "toggle-line"; tog.setAttribute("aria-pressed", String(!lineShown));
  const label = () => (lineShown ? "Hide the line (L)" : "Show the line (L)");
  tog.textContent = label();
  tog.addEventListener("click", toggleLine);
  bar.append(key, tog); card.appendChild(bar);
  const opts = document.createElement("div"); opts.className = "options gutteropts";
  GUTTER_ANSWERS.forEach(([choice, text], k) => {
    const b = document.createElement("button"); b.type = "button"; b.className = "opt"; b.id = "opt" + (k + 1); b.disabled = busy || stale;
    if (prev === choice) b.classList.add("chosen");
    const kk = document.createElement("span"); kk.className = "key"; kk.textContent = `${k + 1}`;
    const big = document.createElement("span"); big.className = "yn"; big.textContent = text;
    b.append(kk, big);
    b.addEventListener("click", () => choose(choice));
    opts.appendChild(b);
  });
  card.appendChild(opts);
}

// The letter key (README.md, "Letter key"): the language's letters as printed in this paper, cut from words both readings
// agree on, away from the line being judged. A task with `key` names a map (a JSON clip) of a sprite of those pictures. It only
// helps the eye: it changes nothing about the readings or the answer, so the display revision stays. Closed by default on
// narrow screens; a visitor's own choice is kept in this browser.
const KEY_FONTS = ["UnifrakturMaguntia", "IM Fell English", "Noto Sans Cherokee"]; // the bundled faces a key may name
const KEY_LEVELS = { page: "this page", issue: "another page of this issue", title: "another issue of this paper", font: "a period typeface; no clean print of it in this paper" };
let keyOpen = null;
const keyMaps = new Map(); // clip key -> Promise of the map, for the words of one page
function keyShown() {
  if (keyOpen === null) { const v = store(KEYOPEN); keyOpen = v === null ? !(window.matchMedia && window.matchMedia("(max-width: 600px)").matches) : v === "1"; }
  return keyOpen;
}
function keyMap(k) {
  if (!keyMaps.has(k)) {
    if (keyMaps.size > 20) keyMaps.delete(keyMaps.keys().next().value);
    const p = getJSON("/clips/" + k);
    p.catch(() => keyMaps.delete(k));
    keyMaps.set(k, p);
  }
  return keyMaps.get(k);
}
// The letters in question: those the highlights mark in either reading, and their equivalents (ſ for s, ꝛ for r).
function disputed(t) {
  const out = new Set();
  for (const side of ["a", "b"]) { const ls = letters(t[side] || ""); for (const i of t[side + "_diff"] || []) if (ls[i] !== undefined) out.add(ls[i]); }
  for (const [k, v] of Object.entries(cfg.equivalents || {})) { if (out.has(k)) out.add(v); if (out.has(v)) out.add(k); }
  return out;
}
function renderKey(card, t) {
  if (!t.key) return;
  const bar = document.createElement("div"); bar.className = "keybar";
  const tog = document.createElement("button"); tog.type = "button"; tog.className = "small"; tog.id = "toggle-key";
  tog.textContent = "Letter key (K)"; tog.setAttribute("aria-controls", "letterkey"); tog.setAttribute("aria-expanded", String(keyShown()));
  tog.addEventListener("click", toggleKey);
  const hint = document.createElement("span"); hint.className = "muted";
  hint.textContent = "This paper's letters, from words both readings agree on. Highlighted: the letters in question.";
  bar.append(tog, hint); card.appendChild(bar);
  const panel = document.createElement("div"); panel.className = "letterkey"; panel.id = "letterkey";
  panel.setAttribute("role", "region"); panel.setAttribute("aria-label", "Letter key: letters as printed in this paper");
  panel.hidden = !keyShown(); card.appendChild(panel);
  if (keyShown()) fillKey(panel, t);
}
async function fillKey(panel, t) {
  panel.dataset.filled = "1"; panel.textContent = "Loading the letter key…";
  let map;
  // A failed load clears the flag, so closing and opening the key tries again.
  try { map = await keyMap(t.key); } catch { delete panel.dataset.filled; if (task === t) panel.textContent = "The letter key didn't load."; return; }
  if (task !== t || !panel.isConnected) return;
  panel.textContent = "";
  if (!map || typeof map.sprite !== "string" || !/^[A-Za-z0-9._\-/]{1,300}$/.test(map.sprite) || map.sprite.includes("..")) { panel.textContent = "The letter key didn't load."; return; }
  const hot = disputed(t), scale = 34 / (map.h || 34);
  const glyphs = Array.isArray(map.glyphs) ? map.glyphs : [];
  const first = glyphs.filter((g) => hot.has(g.g)), rest = glyphs.filter((g) => !hot.has(g.g));
  const item = (g) => {
    const level = KEY_LEVELS[g.level] || "";
    const el = document.createElement("span"); el.className = "kg" + (hot.has(g.g) ? " disputed" : ""); el.title = `${g.g}: from ${level}`;
    el.setAttribute("role", "group"); el.setAttribute("aria-label", `${g.g}${hot.has(g.g) ? ", a letter in question" : ""}, from ${level}`);
    const l = document.createElement("span"); l.className = "kl"; l.lang = lang; l.textContent = g.g; l.setAttribute("aria-hidden", "true"); el.appendChild(l);
    if (g.level === "font") {
      const f = document.createElement("span"); f.className = "kf"; f.lang = lang; f.textContent = g.g; f.setAttribute("role", "img");
      f.setAttribute("aria-label", `${g.g} drawn in a period typeface`);
      if (KEY_FONTS.includes(map.font)) f.style.fontFamily = `"${map.font}", var(--font-script)`;
      el.appendChild(f);
    } else {
      for (const c of (g.crops || []).slice(0, 3)) {
        if (!Array.isArray(c) || c.length < 3) continue;
        const [x, y, w] = c.map(Number);
        const im = document.createElement("span"); im.className = "kc"; im.setAttribute("role", "img"); im.setAttribute("aria-label", `printed ${g.g}, from ${level}`);
        Object.assign(im.style, { width: w * scale + "px", backgroundImage: `url("/clips/${map.sprite}")`, backgroundPosition: `${-x * scale}px ${-y * scale}px`, backgroundSize: `${(map.w || 0) * scale}px auto` });
        el.appendChild(im);
      }
    }
    return el;
  };
  for (const g of first) panel.appendChild(item(g));
  if (first.length && rest.length) { const gap = document.createElement("span"); gap.className = "kgap"; gap.setAttribute("aria-hidden", "true"); panel.appendChild(gap); }
  for (const g of rest) panel.appendChild(item(g));
  if (!glyphs.length) panel.textContent = "No letters of this paper to show.";
}
function toggleKey() {
  if (!task || !task.key) return;
  keyOpen = !keyShown(); store(KEYOPEN, keyOpen ? "1" : "0");
  const panel = $("letterkey"), tog = $("toggle-key");
  if (tog) tog.setAttribute("aria-expanded", String(keyOpen));
  if (panel) { panel.hidden = !keyOpen; if (keyOpen && !panel.dataset.filled) fillKey(panel, task); }
}

// ---------- one newspaper at a time (README.md, "One newspaper at a time") ----------

// A language with `by_paper` serves its words one newspaper at a time, so a volunteer gets used to one paper's type, starting
// with the paper that has the most open words. The visitor can pick another paper, or any. Above the word is the paper's
// letter guide: its letters as printed in that paper, cut from words both readings agree on, merged from the letter keys of
// the paper's pages (README.md, "Letter key"), with the letter pairs the machines confuse (`confusions`) side by side first.
// Like the letter key, it only helps the eye and changes nothing about the readings or the answers.
const PAPERSEL = "usnm-check-paper-"; // + language code: the paper the visitor picked in this browser ("*" for any)
const GUIDEOPEN = "usnm-check-guide"; // "1" or "0": the visitor opened or closed the letter guide in this browser
let papers = [], paper = "", guideOpen = null, guideFor = null;
const done = new Set(); // papers that had no word left for this visitor on this page
const paperInfo = (p = paper) => papers.find((x) => x.paper === p) || null;
const nextPaper = () => papers.find((x) => x.paper !== paper && x.open > 0 && !done.has(x.paper)) || null;
function guideShown() {
  if (guideOpen === null) { const v = store(GUIDEOPEN); guideOpen = v === null ? !(window.matchMedia && window.matchMedia("(max-width: 600px)").matches) : v === "1"; }
  return guideOpen;
}

async function loadPapers() {
  try { papers = await getJSON("/api/papers?lang=" + encodeURIComponent(lang)); } catch { papers = []; }
  if (!Array.isArray(papers)) papers = [];
  papers = papers.filter((x) => x && typeof x.paper === "string" && /^[A-Za-z0-9_-]{1,40}$/.test(x.paper));
  if (!papers.length) return;
  const kept = store(PAPERSEL + lang);
  paper = kept === "*" ? "" : kept && papers.some((x) => x.paper === kept && x.open > 0) ? kept : (papers.find((x) => x.open > 0) || papers[0]).paper;
  renderPaperBar();
}

function renderPaperBar() {
  const bar = $("paperbar"); bar.textContent = "";
  const label = document.createElement("label"); label.htmlFor = "paper"; label.textContent = "Newspaper ";
  const sel = document.createElement("select"); sel.id = "paper";
  for (const x of papers) {
    const o = document.createElement("option"); o.value = x.paper;
    o.textContent = `${x.name} (${x.open.toLocaleString()} open)`; o.selected = x.paper === paper; sel.appendChild(o);
  }
  const any = document.createElement("option"); any.value = "*"; any.textContent = "Any newspaper, mixed"; any.selected = !paper; sel.appendChild(any);
  sel.addEventListener("change", () => setPaper(sel.value === "*" ? "" : sel.value));
  label.appendChild(sel);
  const tog = document.createElement("button"); tog.type = "button"; tog.className = "small"; tog.id = "toggle-guide";
  tog.textContent = "Letter guide (G)"; tog.setAttribute("aria-controls", "guide"); tog.setAttribute("aria-expanded", String(guideShown()));
  tog.hidden = !paper; tog.addEventListener("click", toggleGuide);
  const hint = document.createElement("span"); hint.className = "muted"; hint.id = "paper-hint";
  hint.textContent = paper ? "Words come from this paper only, so you can get used to its type." : "Words come from every paper.";
  const row = document.createElement("div"); row.className = "paperrow"; row.append(label, tog, hint);
  const panel = document.createElement("div"); panel.className = "letterkey guide"; panel.id = "guide";
  panel.setAttribute("role", "region"); panel.setAttribute("aria-label", "Letter guide: letters as printed in this newspaper");
  panel.hidden = !paper || !guideShown();
  bar.append(row, panel); bar.hidden = false;
  guideFor = null;
  if (!panel.hidden) fillGuide(panel);
}

// Leaving the word shown for another paper's: it isn't answered (a word answered Neither joins the history, as Skip does).
function setPaper(p) {
  if (busy || stale) { const sel = $("paper"); if (sel) sel.value = paper || "*"; return; }
  paper = p; store(PAPERSEL + lang, p || "*"); done.delete(p);
  if (task && suggestFor === task.id) { history.push({ task, choice: "neither" }); if (history.length > 50) history.shift(); }
  renderPaperBar();
  next(controlFocused(), null, undefined, false);
}

function toggleGuide() {
  if (!paper) return;
  guideOpen = !guideShown(); store(GUIDEOPEN, guideOpen ? "1" : "0");
  const panel = $("guide"), tog = $("toggle-guide");
  if (tog) tog.setAttribute("aria-expanded", String(guideOpen));
  if (panel) { panel.hidden = !guideOpen; if (guideOpen && guideFor !== paper) fillGuide(panel); }
}

// The paper's letters, from the maps of its pages' letter keys: for each letter up to three pictures, taken in turn from
// different pages, from a page's own print (level "page") first, else what a key took from the paper's other pages, else the
// letter in the period typeface. A letter no map has is left out, except the confusion pairs, drawn in the typeface.
async function fillGuide(panel) {
  const p = paper, info = paperInfo(p);
  guideFor = p; panel.textContent = "Loading the letter guide…";
  const maps = [];
  for (const k of (info ? info.keys : []).slice(0, 12)) {
    try {
      const m = await keyMap(k);
      if (m && typeof m.sprite === "string" && /^[A-Za-z0-9._\-/]{1,300}$/.test(m.sprite) && !m.sprite.includes("..") && Array.isArray(m.glyphs)) maps.push(m);
    } catch { /* a page whose map didn't load gives no pictures */ }
  }
  if (paper !== p || !panel.isConnected) return;
  const order = [], pics = new Map(); // glyph -> {page: [[...from map 0], [...map 1]], other: same}
  let font = null;
  maps.forEach((m, mi) => {
    if (!font && KEY_FONTS.includes(m.font)) font = m.font;
    const scale = 34 / (m.h || 34);
    for (const g of m.glyphs) {
      if (!g || typeof g.g !== "string") continue;
      if (!pics.has(g.g)) { pics.set(g.g, { page: [], other: [] }); order.push(g.g); }
      const crops = (Array.isArray(g.crops) ? g.crops : []).filter((c) => Array.isArray(c) && c.length >= 3).map((c) => ({ m, scale, c: c.map(Number) }));
      if (!crops.length) continue;
      const side = pics.get(g.g)[g.level === "page" ? "page" : "other"];
      side[mi] = (side[mi] || []).concat(crops);
    }
  });
  // In turn from each page: the first picture of every page, then the second, so three pictures show three pages where they can.
  const pick = (lists) => { const out = [], ls = lists.filter(Boolean); for (let i = 0; out.length < 3 && ls.some((l) => i < l.length); i++) for (const l of ls) if (i < l.length && out.length < 3) out.push(l[i]); return out; };
  const cell = (g) => {
    const got = pics.get(g), crops = got ? (pick(got.page).length ? pick(got.page) : pick(got.other)) : [];
    const el = document.createElement("span"); el.className = "kg"; el.dataset.g = g;
    el.setAttribute("role", "group"); el.setAttribute("aria-label", crops.length ? `${g}, as printed in this paper` : `${g}, drawn in a period typeface; no clean print of it in this paper yet`);
    const l = document.createElement("span"); l.className = "kl"; l.lang = lang; l.textContent = g; l.setAttribute("aria-hidden", "true"); el.appendChild(l);
    if (!crops.length) {
      const f = document.createElement("span"); f.className = "kf"; f.lang = lang; f.textContent = g; f.setAttribute("aria-hidden", "true");
      f.style.fontFamily = `"${font || "UnifrakturMaguntia"}", var(--font-script)`; el.appendChild(f);
      el.title = `${g}: a period typeface (no clean print of it in this paper yet)`;
      return el;
    }
    for (const { m, scale, c: [x, y, w] } of crops) {
      const im = document.createElement("span"); im.className = "kc"; im.setAttribute("aria-hidden", "true");
      Object.assign(im.style, { width: w * scale + "px", backgroundImage: `url("/clips/${m.sprite}")`, backgroundPosition: `${-x * scale}px ${-y * scale}px`, backgroundSize: `${(m.w || 0) * scale}px auto` });
      el.appendChild(im);
    }
    el.title = `${g}: printed in this paper`;
    return el;
  };
  panel.textContent = "";
  const pairs = (Array.isArray(cfg.confusions) ? cfg.confusions : []).filter((x) => Array.isArray(x) && x.length === 2);
  if (pairs.length) {
    const h = document.createElement("p"); h.className = "guide-h"; h.textContent = "Letters the machines mix up, as this paper prints them:"; panel.appendChild(h);
    const box = document.createElement("div"); box.className = "guide-pairs";
    for (const [x, y] of pairs) { const pr = document.createElement("span"); pr.className = "guide-pair"; pr.append(cell(x), cell(y)); box.appendChild(pr); }
    panel.appendChild(box);
  }
  if (order.length) {
    const all = document.createElement("details"); all.className = "guide-all";
    const sum = document.createElement("summary"); sum.textContent = "Every letter in this paper"; all.appendChild(sum);
    const box = document.createElement("div"); box.className = "guide-letters";
    for (const g of order) box.appendChild(cell(g));
    all.appendChild(box); panel.appendChild(all);
  } else {
    const n = document.createElement("p"); n.className = "muted"; n.textContent = "No pictures from this paper yet: the letters above are drawn in a period typeface."; panel.appendChild(n);
  }
  if (task) markGuide(task);
}

// Outlines, in the guide, the letters in question in the word shown.
function markGuide(t) {
  const panel = $("guide");
  if (!panel) return;
  const hot = t && !isGutter(t) && !isCover(t) ? disputed(t) : new Set();
  for (const el of panel.querySelectorAll(".kg")) el.classList.toggle("disputed", hot.has(el.dataset.g));
}

function toggleLine() {
  if (!isGutter(task)) return;
  lineShown = !lineShown;
  const ln = $("gline"), tog = $("toggle-line");
  if (ln) ln.hidden = !lineShown;
  if (tog) { tog.textContent = lineShown ? "Hide the line (L)" : "Show the line (L)"; tog.setAttribute("aria-pressed", String(!lineShown)); }
}

function render(prev) {
  shownPrev = prev;
  const card = $("card"); card.textContent = "";
  showProgress();
  $("back").disabled = history.length === 0 || busy;
  const live = !!task && !busy && !fontFailed && !stale;
  $("neither").disabled = $("unsure").disabled = $("notext").disabled = $("badcrop").disabled = $("skip").disabled = !live;
  // A column gutter task has only yes, no and can't tell, and Bad crop, which every task has.
  // A cover task has yes, no, can't tell, not text and Bad crop.
  $("neither").hidden = isGutter(task) || isCover(task);
  $("notext").hidden = isGutter(task);
  if (task) $("badcrop-help").textContent = isGutter(task) ? GUTTER_BADCROP_HELP : isCover(task) ? COVER_BADCROP_HELP : BADCROP_HELP;
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
    // Skipped words are left out for the page's life; they may still be open, so offer them again rather than say none are left.
    if (paper) {
      // One newspaper at a time: this paper has nothing left for this visitor; offer the next one with open words.
      const info = paperInfo(), after = nextPaper();
      d.lastChild.textContent = `You've answered every open word from ${info ? info.name : "this newspaper"} that you were given. Thank you. `;
      if (after) {
        const go = document.createElement("button"); go.type = "button"; go.className = "small"; go.id = "next-paper";
        go.textContent = `Go on to ${after.name}`; go.addEventListener("click", () => setPaper(after.paper));
        d.appendChild(go);
      }
      if (skipped.length) d.appendChild(document.createTextNode(" "));
    }
    if (skipped.length && paper) {
      const again = document.createElement("button"); again.type = "button"; again.className = "small"; again.id = "retry";
      again.textContent = `Show ${skipped.length === 1 ? "1 skipped word" : skipped.length + " skipped words"}`;
      again.addEventListener("click", async () => { const keep = skipped; skipped = []; await next(true); if (failed && !task) { skipped = keep.concat(skipped); render(); } });
      d.appendChild(again);
    } else if (skipped.length) {
      d.lastChild.textContent = `You've answered or skipped every open word you were given. Thank you. ${skipped.length === 1 ? "1 skipped word" : skipped.length + " skipped words"} may still need answers. `;
      const again = document.createElement("button"); again.type = "button"; again.className = "small"; again.id = "retry";
      again.textContent = "Show skipped words"; // A failed fetch puts the skipped words back, so the button stays offered next to the error.
      again.addEventListener("click", async () => { const keep = skipped; skipped = []; await next(true); if (failed && !task) { skipped = keep.concat(skipped); render(); } });
      d.appendChild(again);
    }
    card.appendChild(d); return;
  }
  if (fontFailed) {
    const d = document.createElement("div"); d.className = "done";
    d.textContent = "The Cherokee letters didn't load, so the readings can't be told apart here. ";
    const again = document.createElement("button"); again.type = "button"; again.className = "small"; again.id = "retry";
    again.textContent = "Reload the page"; again.addEventListener("click", () => location.reload());
    d.appendChild(again); card.appendChild(d); return;
  }
  const t = task;
  if (isGutter(t) || isCover(t)) {
    (isGutter(t) ? renderGutter : renderCover)(card, t, prev);
    if (t.source) {
      const src = document.createElement("a"); src.href = t.source; src.target = "_blank"; src.rel = "noopener"; src.className = "label"; src.textContent = "See the whole page";
      card.appendChild(src);
    }
    return;
  }
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
  // The enlarged word is never drawn smaller than the same word in the line above it (a short line is stretched to the card's
  // width, so its word can outgrow the word clip at its own size).
  const fit = () => fitWord(li, wi, wd, w, h);
  li.addEventListener("load", fit); wi.addEventListener("load", fit);
  if (typeof ResizeObserver === "function") new ResizeObserver(fit).observe(lv);
  const opts = document.createElement("div"); opts.className = "options";
  (flip(t.id) ? ["b", "a"] : ["a", "b"]).forEach((side, k) => {
    const b = document.createElement("button"); b.type = "button"; b.className = "opt"; b.id = "opt" + (k + 1); b.disabled = busy || stale;
    if (prev === side) b.classList.add("chosen");
    const key = document.createElement("span"); key.className = "key"; key.textContent = `Reading ${k + 1}  (${k + 1})`;
    const named = namesOn();
    b.append(key, scriptText(t[side], t[side + "_diff"], named), examples(t[side], t[side + "_diff"], lookalikeLabels(t, side), markLabels(t, side), named));
    b.addEventListener("click", () => choose(side));
    opts.appendChild(b);
  });
  card.appendChild(opts);
  renderKey(card, t);
  markGuide(t);
  if (suggestFor === t.id) card.appendChild(suggestBox(t));
  if (t.source) {
    const src = document.createElement("a"); src.href = t.source; src.target = "_blank"; src.rel = "noopener"; src.className = "label"; src.textContent = "See the whole page";
    card.appendChild(src);
  }
}

// Sizes the word clip so the word is never smaller than in the line as drawn. The clip carries padding around the word, so its
// width or its height is held to WORD_OVER_LINE times the word's box in the line on that side, whichever needs the larger clip:
// a clip padded less than that much on either side then shows the word larger than the line does (a short, narrow word padded
// more on both is the rare exception). A clip already larger keeps its own size (at most WORD_MAX_HEIGHT tall); none grows past
// the word strip.
const WORD_OVER_LINE = 1.5, WORD_MAX_HEIGHT = 120;
function fitWord(li, wi, wd, w, h) {
  if (!li.naturalWidth || !wi.naturalWidth || !li.clientWidth) return;
  const own = wi.naturalWidth * Math.min(1, WORD_MAX_HEIGHT / wi.naturalHeight);
  const scale = WORD_OVER_LINE * Math.max(w * li.clientWidth / wi.naturalWidth, h * li.clientHeight / wi.naturalHeight);
  const room = wd.clientWidth - 12; // .word's padding, 6px a side
  const width = Math.min(Math.max(own, wi.naturalWidth * scale), room);
  if (width > 0) Object.assign(wi.style, { width: Math.floor(width) + "px", maxHeight: "none" });
}

// The optional "What does it say?" box after Neither matches. Sending saves the text as extra evidence and then moves on; Skip,
// and any other control, moves on without saving. It never blocks moving on, and it takes no part in the answer.
function suggestBox(t) {
  const box = document.createElement("div"); box.className = "suggest"; box.id = "suggest";
  const label = document.createElement("label"); label.htmlFor = "suggest-text"; label.textContent = "What does it say? (optional)";
  const input = document.createElement("input");
  // No maxLength: it counts UTF-16 units, not the code points the Worker counts after NFC, so it would refuse valid text.
  Object.assign(input, { type: "text", id: "suggest-text", dir: "auto", lang, autocomplete: "off", spellcheck: false });
  input.value = suggestDraft;
  // a box drawn again while its text is being saved stays locked, so nothing typed after the send is silently dropped
  if (suggestSending && suggestSending === suggestOpening) input.readOnly = true;
  input.addEventListener("input", () => { suggestDraft = input.value; });
  input.addEventListener("keydown", (e) => { if (e.key === "Enter" && !e.isComposing) { e.preventDefault(); sendSuggestion(t.id); } });
  // Characters a US keyboard can't type (½ and the other single-character fractions; a language's own letters): each button puts
  // its text in the box at the cursor. A language's `insert_chars` replaces the list (an empty list shows none); an entry may be
  // several code points (a letter with its points), and one that starts with a combining mark is drawn on a dotted circle and
  // named from `marks`. The buttons stay hidden behind a keyboard button until asked for, and this browser remembers the choice.
  // pointerdown is cancelled on every one, so the box keeps focus and a phone keeps its keyboard up.
  const chars = Array.isArray(cfg.insert_chars) ? cfg.insert_chars : INSERT_CHARS;
  let pad = null, toggle = null;
  if (chars.length) {
    pad = document.createElement("div"); pad.className = "suggest-chars"; pad.id = "suggest-chars"; pad.setAttribute("role", "group"); pad.setAttribute("aria-label", "Insert a character");
    if (cfg.dir) pad.dir = cfg.dir;
    // An entry that is itself a list is a row: the keys are laid out as a chart, one row per list, in columns that line up
    // from row to row (Cherokee: a consonant per row, a vowel per column), each key with its `labels` entry under it.
    const rows = chars.some(Array.isArray) ? chars.map((r) => (Array.isArray(r) ? r : [r])) : null;
    const key = (ch) => {
      const mark = /^\p{M}/u.test(ch);
      const b = document.createElement("button"); b.type = "button"; b.className = "small char"; b.lang = lang; b.textContent = mark ? "\u25CC" + ch : ch;
      const name = rows && !mark && nameOf(ch);
      if (name) { const n = document.createElement("small"); n.textContent = name; n.setAttribute("aria-hidden", "true"); b.appendChild(n); }
      b.setAttribute("aria-label", "Insert " + (mark && cfg.marks && cfg.marks[ch] ? cfg.marks[ch] : ch) + (name ? ", " + name : ""));
      b.addEventListener("pointerdown", (e) => e.preventDefault());
      b.addEventListener("click", () => {
        // the opening's save state, not only the input's: a re-render during a save makes a new, writable input
        if (input.readOnly || suggestSending === suggestOpening) return;
        const at = input.selectionStart ?? input.value.length, to = input.selectionEnd ?? at;
        input.setRangeText(ch, at, to, "end");
        suggestDraft = input.value;
        input.focus();
      });
      return b;
    };
    if (rows) {
      pad.classList.add("rows"); pad.style.setProperty("--cols", String(Math.max(...rows.map((r) => r.length))));
      for (const r of rows) { const line = document.createElement("div"); line.className = "suggest-chars-row"; line.append(...r.map(key)); pad.appendChild(line); }
    } else for (const ch of chars) pad.appendChild(key(ch));
    const open = store(CHARSOPEN) === "1";
    pad.hidden = !open;
    toggle = document.createElement("button"); toggle.type = "button"; toggle.className = "small chars-toggle"; toggle.id = "suggest-chars-toggle";
    toggle.textContent = "\u2328\uFE0E"; toggle.title = "Special characters";
    toggle.setAttribute("aria-label", "Special characters"); toggle.setAttribute("aria-controls", "suggest-chars"); toggle.setAttribute("aria-expanded", String(open));
    toggle.addEventListener("pointerdown", (e) => e.preventDefault());
    toggle.addEventListener("click", () => {
      const show = pad.hidden;
      pad.hidden = !show; toggle.setAttribute("aria-expanded", String(show)); store(CHARSOPEN, show ? "1" : "0");
      input.focus();
    });
  }
  const row = document.createElement("div"); row.className = "suggest-buttons";
  const send = document.createElement("button"); send.type = "button"; send.className = "small"; send.id = "suggest-send"; send.textContent = "Send";
  if (input.readOnly) send.disabled = true; // locked with the box during a save
  send.addEventListener("click", () => sendSuggestion(t.id));
  const skipBox = document.createElement("button"); skipBox.type = "button"; skipBox.className = "small"; skipBox.id = "suggest-skip"; skipBox.textContent = "Skip";
  skipBox.addEventListener("click", () => moveOnFromSuggestion(t.id));
  row.append(send, skipBox, ...(toggle ? [toggle] : []));
  box.append(label, input, row, ...(pad ? [pad] : []));
  return box;
}

// An empty box just moves on. A failed save keeps the word and the text, so it can be sent again or skipped. One save at a time:
// Enter or Send during a save does nothing, and the box is locked until the save has answered.
async function sendSuggestion(id) {
  const opening = suggestOpening;
  if (busy || !opening || suggestFor !== id || suggestSending === opening) return;
  const input = $("suggest-text");
  const text = input ? input.value.normalize("NFC").trim() : "";
  if (!text) { moveOnFromSuggestion(id); return; }
  if (Array.from(text).length > SUGGEST_MAX) { status(`A suggestion is at most ${SUGGEST_MAX} characters.`, true); return; }
  const send = $("suggest-send");
  suggestSending = opening; if (send) send.disabled = true; if (input) input.readOnly = true;
  try {
    await authed("/api/suggestions", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ task: id, text }) });
  } catch (e) {
    if (suggestOpening === opening) {
      // the box drawn now, which a re-render during the save (letter names toggled) may have replaced
      suggestSending = null;
      const nowSend = $("suggest-send"), nowInput = $("suggest-text");
      if (nowSend) nowSend.disabled = false; if (nowInput) nowInput.readOnly = false;
      status("Your suggestion wasn't saved: " + e.message, true);
    }
    return;
  } finally { if (suggestSending === opening) suggestSending = null; }
  if (suggestOpening === opening) moveOnFromSuggestion(id);
}

// The word was answered Neither; it joins the history now, once, as it leaves the screen (choose() doesn't add it while the box is open).
function moveOnFromSuggestion(id) {
  if (suggestFor !== id || busy) return;
  history.push({ task, choice: "neither" }); if (history.length > 50) history.shift();
  next(controlFocused(), id, "neither");
}

// Keyboard users keep their place: when an answer control had focus, the next word's first reading gets it.
function keepFocus(had) {
  if (!had) return;
  const b = $("opt1") || $("retry") || $("back");
  if (b && !b.disabled) b.focus({ preventScroll: true });
}

// The suggestion box counts too: it takes focus after Neither, so a word left from it with Enter hands focus to the next one.
function controlFocused() { const a = document.activeElement; return !!a && (a.tagName === "BUTTON" || a.id === "suggest-text"); }

function showProgress() {
  const n = pending.length;
  let text = answered ? `${answered} answered` : "";
  if (n) text += `${text ? " · " : ""}${n === 1 ? "1 answer" : n + " answers"} ${pending[0].tries ? "waiting for the connection" : "saving"}…`;
  // A refused answer stays noted here, where moving to the next word doesn't clear it.
  if (refused) text += `${text ? " · " : ""}${refused === 1 ? "1 answer" : refused + " answers"} refused, not saved`;
  $("progress").textContent = text;
}

// The clips a word shows, fetched ahead so the next word appears with its pictures.
function clipKeys(t) { return !t ? [] : isGutter(t) ? [t.crop] : isCover(t) ? [t.pageview, t.block].filter(Boolean) : [t.line, t.word].filter(Boolean); }
// Pictures fetched ahead are held until they load: some mobile browsers cancel the download of an Image that is collected.
const held = new Map();
function preload(t) {
  for (const k of clipKeys(t)) {
    if (held.has(k)) continue;
    const im = new Image();
    im.onload = im.onerror = () => { if (held.get(k) === im) held.delete(k); };
    held.set(k, im);
    im.src = "/clips/" + k;
    if (im.decode) im.decode().catch(() => {});
  }
}
// Let go of pictures for words neither shown nor ahead.
function pruneHeld() {
  const keep = new Set([task, ...(ahead ? ahead.list.map((x) => x.task) : [])].flatMap(clipKeys));
  for (const k of [...held.keys()]) if (!keep.has(k)) held.delete(k);
}

// The Worker reads at most 50 ids to skip, so the words this page must not be given again come first. While more answers
// wait to be saved than fit, the fetch waits for saves, so a word with an unsaved answer is never served again.
const SKIP_MAX = 50;
async function fetchNext(also = []) {
  while (new Set([...also, ...pending.map((p) => p.id)]).size > SKIP_MAX) {
    if (stale) throw new Error("This page is out of date.");
    await new Promise((ok) => drained.push(ok));
  }
  const q = new URLSearchParams({ lang });
  if (paper) q.set("paper", paper);
  const skip = [...new Set([...also, ...pending.map((p) => p.id), ...skipped.slice().reverse()])].slice(0, SKIP_MAX);
  if (skip.length) q.set("skip", skip.join(","));
  return authed("/api/next?" + q);
}

// Up to AHEAD words are fetched to follow the word shown, so answering can move on without waiting for the network.
// `ahead` is { forId, list }: forId is the word shown, list the words to follow it, in order, each { p, task, dead }.
const AHEAD = 2;
function dropAhead(keep = []) {
  if (ahead) for (const x of ahead.list) if (!keep.includes(x)) x.dead = true;
  ahead = null;
}
// A word ahead is fetched once the one before it has arrived, skipping the word shown and those before it.
function aheadItem(forId, prior) {
  const item = { task: undefined, dead: false }, last = prior[prior.length - 1];
  item.p = (last ? last.p : Promise.resolve(null)).then((r) => {
    if (item.dead || stale) throw new Error("not needed");
    if (r && !r.task) return r; // no more words
    return fetchNext([forId, ...prior.filter((x) => x.task).map((x) => x.task.id)]);
  }).then((r) => { item.task = r.task; if (!item.dead) preload(r.task); return r; });
  item.p.catch(() => { /* fetched again when needed */ });
  return item;
}
function prefetch(keep = []) {
  dropAhead(keep);
  if (task && !stale) {
    ahead = { forId: task.id, list: keep };
    while (ahead.list.length < AHEAD) ahead.list.push(aheadItem(task.id, ahead.list.slice()));
  } else for (const x of keep) x.dead = true;
  pruneHeld();
}

// Once the page is out of date, its reload message stays in place of news about the next word.
function navStatus(text, error) { if (!stale && !(checking && !error)) status(text, error); }

// `after` is the word being left: the first word fetched ahead for it is shown; with `keepRest`, the others stay ahead.
async function next(had = controlFocused(), after = null, pressed, keepRest = true) {
  suggestFor = null; suggestDraft = ""; suggestOpening = null;
  const list = after && ahead && ahead.forId === after ? ahead.list : [];
  const first = list[0], rest = keepRest ? list.slice(1) : [];
  dropAhead(first ? [first, ...rest] : []);
  let used = false;
  busy = true; render(pressed);
  clearTimeout(slowTimer);
  slowTimer = setTimeout(() => { if (busy) navStatus("Loading the next word…"); }, 400);
  try {
    const r = first ? await first.p.then((x) => { used = true; return x; }, () => fetchNext()) : await fetchNext();
    // A new word or crop starts with its line drawn: "Hide the line" applies to one crop only.
    task = r.task; lineShown = true; answered = Math.max(answered, r.answered); loaded = true; failed = false; navStatus("");
    if (!task && paper) done.add(paper);
    fontFailed = !(await fontReady(task));
    // The loading message may have appeared while the font loaded; the word is ready now, so it goes.
    navStatus(fontFailed ? "The font for this word didn't load." : "", fontFailed);
  } catch (e) {
    failed = true;
    navStatus(e.status === 429 ? "Slow down a little, then try again." : "The next word didn't load. " + e.message, true);
  } finally {
    clearTimeout(slowTimer);
    busy = false; render(); keepFocus(had);
    const keep = used && !failed && !fontFailed ? rest : [];
    for (const x of rest) if (!keep.includes(x)) x.dead = true;
    if (!failed && !fontFailed) prefetch(keep); else pruneHeld();
  }
}

// Answers are saved one at a time, in the order given. A failed save waits and tries again, so a bad connection
// delays answers but doesn't lose them while the page stays open.
async function sendAnswers() {
  if (sending) return;
  sending = true;
  try {
    while (pending.length && !stale) {
      const p = pending[0];
      try {
        inflight = p;
        try {
          await authed("/api/answers", { method: "POST", headers: { "content-type": "application/json" }, body: p.body });
        } finally { inflight = null; }
        unqueue(p);
      } catch (e) {
        // A checked word of a blind set takes no more answers (someone else settled it); the answer is not needed.
        if (e.status === 409) { unqueue(p); continue; }
        if (e.status === 426) {
          stale = true; dropAhead(); pruneHeld(); render(); unqueue(null);
          status(`This page is out of date. Reload it, then answer again. ${pending.length === 1 ? "1 answer wasn't" : pending.length + " answers weren't"} saved.`, true);
          break;
        }
        if (e.status === 400) { unqueue(p); refused++; status("An answer was refused: " + e.message, true); continue; }
        // A changed answer for this word is already queued behind it: send that instead of retrying the old one first.
        if (pending.some((q) => q !== p && q.id === p.id)) { unqueue(p); continue; }
        p.tries++;
        showProgress();
        await new Promise((ok) => { wake = ok; setTimeout(ok, e.status === 429 ? 5000 : Math.min(30000, 1000 * 2 ** Math.min(p.tries, 5))); });
        wake = null;
      }
      showProgress();
    }
  } finally { sending = false; showProgress(); }
}

// Take a saved (or no longer needed) answer off the queue, and let a fetch waiting for room in the skip list go on.
function unqueue(p) {
  const k = pending.indexOf(p);
  if (k >= 0) pending.splice(k, 1);
  for (const ok of drained.splice(0)) ok();
}

addEventListener("beforeunload", (e) => { if (pending.length) { e.preventDefault(); e.returnValue = ""; } });
addEventListener("online", () => { if (wake) wake(); else sendAnswers(); });

async function choose(choice) {
  if (!task || busy || fontFailed || stale) return;
  if (isGutter(task) && (choice === "neither" || choice === "notext")) return;
  if (isCover(task) && choice === "neither") return;
  if (choice === "neither" && suggestFor === task.id) return;
  const had = controlFocused();
  const body = JSON.stringify({ task: task.id, choice, display: DISPLAY, font: fontState(task), names_shown: namesState(task) });
  const id = task.id;
  // Neither keeps the word on screen for the optional box; it joins the history when the box is sent or skipped.
  const holds = choice === "neither" && !isGutter(task) && !isCover(task);
  if (!holds) { history.push({ task, choice }); if (history.length > 50) history.shift(); }
  // A changed answer (Back, then another choice) replaces the one still waiting to be saved, unless it is being sent now.
  const queued = pending.findIndex((p) => p.id === id && p !== inflight);
  if (queued >= 0) pending.splice(queued, 1);
  if (!counted.has(id)) { counted.add(id); answered++; }
  pending.push({ id, body, tries: 0 });
  sendAnswers();
  // Neither keeps the word on screen for the optional box; it moves on when the box is sent or skipped (or another control is used).
  // The cursor goes to the box, by click, tap or key 3, so what the print says can be typed straight away.
  if (holds) { suggestFor = id; suggestOpening = {}; suggestDraft = ""; render(); const box = $("suggest-text"); if (box) box.focus(); return; }
  await next(had, id, choice);
}

function back() {
  if (!history.length || busy || stale) return;
  suggestFor = null; suggestDraft = ""; suggestOpening = null;
  const had = controlFocused();
  const h = history.pop(); task = h.task; lineShown = true; render(h.choice); keepFocus(had); prefetch();
  status("Change your answer, or pick the same one to keep it.");
}

function skip() {
  if (!task || busy || stale) return;
  // A word answered Neither joins the history as it is left, as it does when its box is sent or skipped.
  if (suggestFor === task.id) { history.push({ task, choice: "neither" }); if (history.length > 50) history.shift(); }
  skipped.push(task.id); next(controlFocused(), task.id, undefined, false);
}

async function work(code) {
  lang = code;
  $("work").hidden = false;
  // A first visit has a browser check to run before any word can be fetched: start it now, alongside the language's
  // configuration, instead of after it. next() waits on the same check (session() shares one attempt); a failure here is
  // reported, and retried, when next() asks again.
  session().catch(() => {});
  try { cfg = await getJSON("/api/languages/" + code); }
  catch { $("card").textContent = "We don't have this language. "; const a = document.createElement("a"); a.href = "/"; a.textContent = "See the list"; $("card").appendChild(a); return; }
  document.title = `${cfg.name}: USNewsMap letter check`;
  $("intro").textContent = cfg.intro || "";
  // Further reading under the intro (`links`): https addresses only, each opening in a new tab.
  const links = (Array.isArray(cfg.links) ? cfg.links : []).filter((l) => l && typeof l.text === "string" && /^https:\/\/[^\s"<>]+$/.test(l.href || ""));
  if (links.length) {
    const p = $("intro-links"); p.textContent = "Further reading: ";
    links.forEach((l, i) => {
      if (i) p.appendChild(document.createTextNode(" · "));
      const a = document.createElement("a"); a.href = l.href; a.target = "_blank"; a.rel = "noopener"; a.textContent = l.text; p.appendChild(a);
    });
    p.hidden = false;
  }
  // A language may carry its own rules text and a list of notes for edge cases (the column gutter language does); the others use the page's.
  if (typeof cfg.rules === "string" && cfg.rules) $("rules").textContent = cfg.rules;
  if (Array.isArray(cfg.guidance) && cfg.guidance.length) {
    const ul = $("guidance"); ul.textContent = "";
    for (const g of cfg.guidance) { const li = document.createElement("li"); li.textContent = g; ul.appendChild(li); }
    ul.hidden = false;
  }
  $("download-link").href = "/api/export?lang=" + encodeURIComponent(code); $("download").hidden = false;
  $("neither").addEventListener("click", () => choose("neither"));
  $("unsure").addEventListener("click", () => choose("unsure"));
  $("notext").addEventListener("click", () => choose("notext"));
  $("badcrop").addEventListener("click", () => choose("badcrop"));
  $("back").addEventListener("click", back);
  if (hasNames()) {
    // The option stays off unless this browser's stored choice says "1"; a browser without storage keeps it for the page's life.
    showNames = store(NAMES) === "1";
    const box = $("names"); box.checked = showNames;
    box.addEventListener("change", () => { showNames = box.checked; store(NAMES, showNames ? "1" : "0"); render(shownPrev); });
    $("names-wrap").hidden = false;
  }
  $("skip").addEventListener("click", skip);
  document.addEventListener("keydown", (e) => {
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    if (e.target && e.target.id === "suggest-text") return;
    if (e.target && e.target.tagName === "SELECT") return; // the newspaper picker: its own keys pick a paper
    if (e.key === "1" || e.key === "2") { const b = $("opt" + e.key); if (b) b.click(); }
    // Key 3 moves the cursor into the box it opens: preventDefault keeps the key from typing a "3" there.
    else if (e.key === "3") { e.preventDefault(); choose("neither"); }
    else if (e.key === "4") choose("unsure");
    else if (e.key === "5") choose("notext");
    else if (e.key === "6") choose("badcrop");
    else if (e.key === "s" || e.key === "S") skip();
    else if (e.key === "l" || e.key === "L") toggleLine();
    else if (e.key === "k" || e.key === "K") toggleKey();
    else if (e.key === "g" || e.key === "G") toggleGuide();
    else if (e.key === "ArrowLeft" || e.key === "Backspace") back();
  });
  if (cfg.by_paper) await loadPapers();
  await next();
}

const code = location.pathname.replace(/^\/+|\/+$/g, "");
if (/^[a-z]{2,3}(-[A-Za-z0-9]{2,8})*$/.test(code)) work(code); else home();
