pub(super) const STYLE: &str = r#"
:root { --surface:#fcfcfb; --plane:#f9f9f7; --ink:#0b0b0b; --ink2:#52514e; --muted:#898781;
  --grid:#e1e0d9; --track:#e1e0d9; --fill:#2a78d6; --good:#0ca30c; --warning:#fab219; --critical:#d03b3b;
  --ring:rgba(11,11,11,0.10); }
@media (prefers-color-scheme: dark) { :root { --surface:#1a1a19; --plane:#0d0d0d; --ink:#ffffff; --ink2:#c3c2b7;
  --grid:#2c2c2a; --track:#383835; --fill:#3987e5; --ring:rgba(255,255,255,0.10); } }
* { box-sizing: border-box; }
body { margin:0; padding:16px 20px 40px; background:var(--plane); color:var(--ink);
  font: 14px/1.45 system-ui, -apple-system, "Segoe UI", sans-serif; }
h1 { font-size:20px; margin:0 0 4px; } h2 { font-size:16px; margin:0 0 10px; } h3 { font-size:14px; margin:14px 0 6px; }
.sub { color:var(--ink2); margin-bottom:16px; }
.stale { color:var(--critical); }
/* Panels flow down columns at least 560px wide, each as tall as its own
   content, so a short panel never stretches beside a tall one. */
.grid { columns:560px; column-gap:16px; }
section { background:var(--surface); border:1px solid var(--ring); border-radius:8px; padding:14px 16px; overflow-x:auto;
  break-inside:avoid; margin:0 0 16px; }
section > :last-child { margin-bottom:0; }
/* The diagrams need the page's width. */
section.wide { column-span:all; }
section.wide > details > summary { cursor:pointer; font-weight:600; }
section.wide > details > summary h2 { display:inline; }
section.wide figure { overflow-x:auto; }
section.wide svg { margin:6px 0 10px; min-width:760px; }
.pipeline-table { table-layout:fixed; }
.pipeline-table th:first-child { width:52%; }
.pipeline-table th:nth-child(2) { width:16%; }
.pipeline-table th:nth-child(3) { width:16%; }
.pipeline-table td { white-space:normal; overflow-wrap:anywhere; }
.pipeline-table details > summary { cursor:pointer; }
.pipeline-table details p { margin:6px 0; color:var(--ink2); }
.pipeline-meta { margin-top:12px; }
.pipeline-meta > summary { cursor:pointer; color:var(--ink2); }
.step-rates { table-layout:fixed; }
.step-rates th, .step-rates td { white-space:normal; overflow-wrap:anywhere; }
.step-rates th:first-child { width:25%; }
.step-rates th:nth-child(2) { width:20%; }
.mobile-flow { display:none; }
.diagram-hint { display:none; }
.sub, .ecosystem-card, .kv td { overflow-wrap:anywhere; }
.ecosystem-cards { display:grid; grid-template-columns:repeat(auto-fit,minmax(min(100%,380px),1fr)); gap:14px; }
.ecosystem-card { border:1px solid var(--grid); border-radius:6px; padding:14px; min-width:0; }
.ecosystem-card h3 { margin:0 0 8px; font-size:16px; }
.ecosystem-card p { margin:8px 0; }
.ecosystem-card .headline { font-size:22px; font-weight:650; }
.ecosystem-card .note { margin-bottom:0; }
.diagnostics > summary { cursor:pointer; font-weight:600; }
.diagnostics > .grid { margin-top:16px; }
.sr { position:absolute; width:1px; height:1px; margin:-1px; overflow:hidden; clip-path:inset(50%); white-space:nowrap; }
/* Long lists scroll inside their panel, under a header that stays put. */
.scroll { max-height:360px; overflow:auto; border-bottom:1px solid var(--grid); }
.scroll th { position:sticky; top:0; background:var(--surface); box-shadow:inset 0 -1px var(--grid); z-index:1; }
.scroll tr:last-child td { border-bottom:none; }
/* Cut text opens to the whole of it: an error in a list is two lines at most. */
.more > summary { cursor:pointer; list-style:none; }
.more > summary::-webkit-details-marker { display:none; }
.more[open] > summary > * { display:none; }
.more[open] > summary::after { content:"Show less"; color:var(--fill); }
.more > div { white-space:normal; overflow-wrap:anywhere; max-width:40em; }
td.msg { width:100%; max-width:0; white-space:normal; color:var(--ink2); font-size:13px; }
.clamp { display:-webkit-box; -webkit-box-orient:vertical; -webkit-line-clamp:2; line-clamp:2; overflow:hidden;
  overflow-wrap:anywhere; }
table { border-collapse:collapse; width:100%; font-variant-numeric: tabular-nums; }
th, td { text-align:left; padding:3px 8px 3px 0; border-bottom:1px solid var(--grid); vertical-align:top; white-space:nowrap; }
th { color:var(--ink2); font-weight:600; }
td.r, th.r { text-align:right; }
.muted, .note { color:var(--ink2); } .note { white-space:normal; font-size:13px; }
code { font-size:12.5px; overflow-wrap:anywhere; } a { color:var(--fill); }
.meter { display:inline-block; width:140px; height:8px; background:var(--track); border-radius:4px; vertical-align:middle; overflow:hidden; }
.meter > span { display:block; height:100%; background:var(--fill); border-radius:4px; }
.num { color:var(--ink2); }
.badge { display:inline-block; padding:0 6px; border-radius:4px; border:1px solid var(--ring); font-weight:600; }
.badge.good > span { color:var(--good); } .badge.warning > span { color:var(--warning); } .badge.critical > span { color:var(--critical); }
.kv td:first-child { color:var(--ink2); width:1%; }
.kv td + td { white-space:normal; }
.clip { display:inline-block; max-width:16em; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; vertical-align:bottom; }
.err { color:var(--critical); white-space:normal; overflow-wrap:anywhere; }
/* Phones: one column, no page-wide scroll. Two-column tables become
   label-over-value cards; wider tables wrap, and scroll inside their panel
   if they still don't fit. */
@media (max-width: 640px) {
  body { padding:10px 10px 32px; }
  section { padding:10px 12px; }
  section { margin-bottom:10px; }
  /* Lists keep a row a line or two tall and scroll sideways in their box. */
  .scroll { max-height:320px; }
  .scroll td { white-space:nowrap; } .scroll td code { word-break:normal; }
  .scroll .pipeline-table td { white-space:normal; }
  .pipeline-table { font-size:13px; }
  .pipeline-table th:first-child { width:44%; }
  .pipeline-table th:nth-child(2) { width:20%; }
  .pipeline-table th:nth-child(3) { width:22%; }
  .pipeline-table td:nth-child(3) { overflow-wrap:normal; font-size:12px; }
  .scroll td.msg { white-space:normal; width:auto; max-width:none; min-width:15em; }
  th { white-space:nowrap; padding-right:6px; }
  td { white-space:normal; overflow-wrap:break-word; padding-right:6px; }
  td code { word-break:break-all; }
  table.kv, table.kv tbody, table.kv tr, table.kv td { display:block; width:auto; }
  table.kv td:first-child { width:auto; border-bottom:none; padding:6px 0 0; font-weight:600; }
  table { width:100%; table-layout:auto; }
  table.kv td + td { padding-top:0; }
  .meter { width:84px; }
  .grid { columns:auto; }
  section { min-width:0; }
  #operational-details { padding:10px 8px; }
  .ecosystem-card { padding:12px; }
  .ecosystem-card .headline { font-size:19px; }
  #pipelines figure, #pipelines .diagram-state { display:none; }
  .mobile-flow { display:block; list-style:none; padding:0 0 0 10px; margin:12px 0; border-left:2px solid var(--fill); }
  .mobile-flow li { position:relative; padding:10px 12px; margin:0 0 12px; border:1px solid var(--grid); border-radius:6px; }
  .mobile-flow li + li::before { content:"↓"; position:absolute; top:-17px; left:12px; color:var(--fill); }
  .mobile-flow h4 { font-size:14px; margin:0 0 5px; }
  .mobile-flow p { margin:0; overflow-wrap:anywhere; }
  .diagram-hint { display:block; }
  .step-rates { table-layout:fixed; font-size:12px; }
  .step-rates th:first-child { width:28%; }
  .step-rates th:nth-child(2) { width:24%; }
  .step-rates th, .step-rates td { white-space:normal; overflow-wrap:anywhere; }
  .pipeline-table th { white-space:normal; overflow-wrap:anywhere; }
}
"#;

/// Turns each `<time>` into US Eastern time, then keeps the page
/// current: every `data-refresh` seconds it fetches the page again in the
/// background and swaps in only the panels that changed, keeping each
/// list's scroll position, open rows and any text being selected, so
/// nothing flashes. A failed fetch leaves the old data and says so.
pub(super) const SCRIPT: &str = r##"
const localTimes = (root) => {
  for (const t of root.querySelectorAll('time[datetime]')) {
    const d = new Date(t.getAttribute('datetime'));
    if (!isNaN(d)) t.textContent = d.toLocaleString('en-US', {timeZone:'America/New_York', month:'short', day:'numeric', hour:'2-digit', minute:'2-digit', second:'2-digit', timeZoneName:'short'});
  }
};
const panelGroups = ['.dashboard', '.diagnostics > .grid'];
const childPanels = (grid) => [...grid.children].filter(s => s.matches('section:not(#operational-details)'));
const panels = (root = document) => panelGroups.flatMap(selector => childPanels(root.querySelector(selector)));
const titleOf = (s) => (s.querySelector('h2') || {}).textContent || '';
const panelKey = (s) => s.id || titleOf(s);
let rawSub = (document.querySelector('.sub time') || {outerHTML: ''}).outerHTML;
const raw = new WeakMap(panels().map(s => [s, s.outerHTML]));
localTimes(document);

const selecting = (el) => {
  const sel = window.getSelection();
  return !!sel && !sel.isCollapsed && sel.containsNode(el, true);
};
const rowKeys = (panel) => {
  const texts = [...panel.querySelectorAll('details')].map((d) => d.dataset.refreshKey || (d.querySelector('summary') || {}).textContent || '');
  return texts.map((t, i) => texts.indexOf(t) === texts.lastIndexOf(t) ? t : i + ':' + t);
};
const positions = new WeakMap();
const setPosition = (el, top, left) => {
  el.scrollTop = top;
  el.scrollLeft = left;
  positions.set(el, [top, left]);
};
const swap = (old, fresh) => {
  const scrolls = [...old.querySelectorAll('.scroll')].map((e) => [e.getAttribute('aria-label'), e.scrollTop, e.scrollLeft]);
  const open = rowKeys(old).filter((k, i) => old.querySelectorAll('details')[i].open).filter(Boolean);
  const focused = old.contains(document.activeElement) ? document.activeElement.getAttribute('aria-label') : null;
  const focusedDetail = [...old.querySelectorAll('details')].findIndex(d => d.querySelector(':scope > summary') === document.activeElement);
  const focusedKey = focusedDetail < 0 ? null : rowKeys(old)[focusedDetail];
  const left = old.scrollLeft;
  const node = document.importNode(fresh, true);
  localTimes(node);
  const keys = rowKeys(node);
  node.querySelectorAll('details').forEach((d, i) => { if (open.includes(keys[i])) d.open = true; });
  old.replaceWith(node);
  const restore = () => {
    node.getBoundingClientRect();
    setPosition(node, 0, left);
    for (const [label, top, lf] of scrolls) {
      const e = [...node.querySelectorAll('.scroll')].find((x) => x.getAttribute('aria-label') === label);
      if (e) setPosition(e, top, lf);
    }
  };
  restore();
  const focus = [...node.querySelectorAll('.scroll')].find(e => e.getAttribute('aria-label') === focused);
  if (focus) focus.focus({preventScroll:true});
  if (focusedKey) node.querySelectorAll('details')[keys.indexOf(focusedKey)]?.querySelector(':scope > summary')?.focus({preventScroll:true});
  requestAnimationFrame(() => { if (node.isConnected && node.checkVisibility()) restore(); });
  return node;
};

let busy = false;
let restoringDiagnostics = false;
const update = async () => {
  if (busy || document.hidden) return;
  busy = true;
  const sub = document.querySelector('.sub');
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(), 20000);
  try {
    const r = await fetch(location.pathname + location.search, {cache:'no-store', signal:ctl.signal});
    if (!r.ok) throw new Error('status ' + r.status);
    const next = new DOMParser().parseFromString(await r.text(), 'text/html');
    if (panelGroups.some(selector => !next.querySelector(selector))) throw new Error('not the dashboard');
    const then = panels(next);
    const nextSub = next.querySelector('.sub');
    if (!nextSub || !then.length) throw new Error('not the dashboard');
    const t = sub.querySelector('time'), nt = nextSub.querySelector('time');
    if (t && nt && nt.outerHTML !== rawSub) {
      t.setAttribute('datetime', nt.getAttribute('datetime'));
      t.textContent = nt.textContent;
      rawSub = nt.outerHTML;
      localTimes(sub);
    }
    sub.querySelector('.stale')?.remove();
    for (const selector of panelGroups) {
      if (selector === '.diagnostics > .grid' &&
          (!document.querySelector('.diagnostics').open || restoringDiagnostics)) continue;
      const grid = document.querySelector(selector);
      const old = childPanels(grid);
      const freshPanels = childPanels(next.querySelector(selector));
      if (selecting(grid) && (old.length !== freshPanels.length ||
          old.some((s, i) => panelKey(s) !== panelKey(freshPanels[i])))) continue;
      const kept = freshPanels.map(fresh => {
        const current = old.find(s => panelKey(s) === panelKey(fresh));
        if (current && (raw.get(current) === fresh.outerHTML || selecting(current))) return current;
        const node = current ? swap(current, fresh) : document.importNode(fresh, true);
        raw.set(node, fresh.outerHTML);
        if (!current) localTimes(node);
        return node;
      });
      let cursor = grid.firstElementChild;
      for (const node of kept) {
        if (node !== cursor) grid.insertBefore(node, cursor);
        cursor = node.nextElementSibling;
      }
      for (const node of old) if (!kept.includes(node)) node.remove();
    }
  } catch (e) {
    if (!sub.querySelector('.stale')) {
      const s = document.createElement('span');
      s.className = 'stale';
      s.textContent = ' Could not refresh; showing the last data.';
      sub.append(s);
    }
  } finally {
    clearTimeout(timer);
    busy = false;
  }
};
const diagnostics = document.querySelector('.diagnostics');
// Closed details may reset descendant offsets even without replacing the elements.
const diagnosticScrolls = () => diagnostics.querySelectorAll('section, .scroll');
diagnostics.querySelector(':scope > summary').addEventListener('click', () => {
  if (diagnostics.open) diagnosticScrolls().forEach(el => positions.set(el, [el.scrollTop, el.scrollLeft]));
});
document.addEventListener('scroll', event => {
  const el = event.target;
  if (diagnostics.open && !restoringDiagnostics && el instanceof Element && diagnostics.contains(el) && el.matches('section, .scroll'))
    positions.set(el, [el.scrollTop, el.scrollLeft]);
}, true);
new MutationObserver(() => {
  if (!diagnostics.open) return;
  restoringDiagnostics = true;
  requestAnimationFrame(() => {
    if (diagnostics.open) diagnosticScrolls().forEach(el => {
      const saved = positions.get(el);
      if (saved) { el.getBoundingClientRect(); setPosition(el, ...saved); }
    });
    restoringDiagnostics = false;
    if (diagnostics.open) update();
  });
}).observe(diagnostics, {attributes:true, attributeFilter:['open']});
const every = Math.max(5, Number(document.body.dataset.refresh) || 30) * 1000;
setInterval(update, every);
document.addEventListener('visibilitychange', () => { if (!document.hidden) update(); });
"##;
