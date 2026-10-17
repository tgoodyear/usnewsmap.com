//! The page: one server-rendered HTML document. Its script fetches it again
//! on a timer and swaps in the panels that changed.
//! Every value from outside goes through [`esc`].

use std::fmt::Write as _;

use chrono::{DateTime, Utc};

use crate::view::{NextRow, View};

/// HTML-escape text for an element or a quoted attribute.
pub fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            c => o.push(c),
        }
    }
    o
}

/// Bytes in decimal units.
pub fn bytes(n: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1000.0 && u < units.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else if v >= 100.0 {
        format!("{v:.0} {}", units[u])
    } else {
        format!("{v:.1} {}", units[u])
    }
}

/// A span of time: `2d 4h`, `3h 12m`, `5m 3s`, `40s`.
pub fn span(secs: u64) -> String {
    let (d, h, m, s) = (
        secs / 86400,
        secs % 86400 / 3600,
        secs % 3600 / 60,
        secs % 60,
    );
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

pub fn int(n: u64) -> String {
    let s = n.to_string();
    let mut o = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            o.push(',');
        }
        o.push(c);
    }
    o
}

fn pct(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        (part as f64 / whole as f64 * 100.0).min(100.0)
    }
}

/// A time, in US Eastern once the page's script runs, UTC before.
fn time(t: DateTime<Utc>) -> String {
    let iso = t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    format!(
        "<time datetime=\"{iso}\">{}</time>",
        t.format("%Y-%m-%d %H:%M:%S UTC")
    )
}

/// [`time`] for an RFC 3339 string; the string itself if it isn't one.
fn time_str(s: &str) -> String {
    match DateTime::parse_from_rfc3339(s) {
        Ok(t) => time(t.with_timezone(&Utc)),
        Err(_) => esc(s),
    }
}

fn ago(now: DateTime<Utc>, t: DateTime<Utc>) -> String {
    let s = (now - t).num_seconds().max(0) as u64;
    format!("{} ago", span(s))
}

/// A short form that opens, by click, tap or keyboard, to the whole of it.
fn more(short: &str, whole: &str) -> String {
    format!("<details class=\"more\"><summary>{short}</summary><div>{whole}</div></details>")
}

/// Text cut to one line with an ellipsis, opening to the whole of it.
fn clip(s: &str) -> String {
    more(&format!("<span class=\"clip\">{}</span>", esc(s)), &esc(s))
}

/// A progress track with its label beside it.
fn meter(part: u64, whole: u64, label: &str) -> String {
    let p = pct(part, whole);
    format!(
        "<span class=\"meter\" role=\"meter\" aria-valuemin=\"0\" aria-valuemax=\"100\" aria-valuenow=\"{p:.1}\"><span style=\"width:{p:.2}%\"></span></span> <span class=\"num\">{}</span>",
        esc(label)
    )
}

/// A state with its icon and word, never color alone.
fn badge(kind: &str, text: &str) -> String {
    let icon = match kind {
        "good" => "&#10003;",
        "warning" => "!",
        "critical" => "&#10007;",
        _ => "&#8226;",
    };
    format!(
        "<span class=\"badge {kind}\"><span aria-hidden=\"true\">{icon}</span> {}</span>",
        esc(text)
    )
}

const STYLE: &str = r#"
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
}
"#;

/// Turns each `<time>` into US Eastern time, then keeps the page
/// current: every `data-refresh` seconds it fetches the page again in the
/// background and swaps in only the panels that changed, keeping each
/// list's scroll position, open rows and any text being selected, so
/// nothing flashes. A failed fetch leaves the old data and says so.
const SCRIPT: &str = r##"
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
const swap = (old, fresh) => {
  const scrolls = [...old.querySelectorAll('.scroll')].map((e) => [e.getAttribute('aria-label'), e.scrollTop, e.scrollLeft]);
  const open = rowKeys(old).filter((k, i) => old.querySelectorAll('details')[i].open).filter(Boolean);
  const focused = old.contains(document.activeElement) ? document.activeElement.getAttribute('aria-label') : null;
  const left = old.scrollLeft;
  const node = document.importNode(fresh, true);
  localTimes(node);
  const keys = rowKeys(node);
  node.querySelectorAll('details').forEach((d, i) => { if (open.includes(keys[i])) d.open = true; });
  old.replaceWith(node);
  node.scrollLeft = left;
  for (const [label, top, lf] of scrolls) {
    const e = [...node.querySelectorAll('.scroll')].find((x) => x.getAttribute('aria-label') === label);
    if (e) { e.scrollTop = top; e.scrollLeft = lf; if (label === focused) e.focus({preventScroll:true}); }
  }
  return node;
};

let busy = false;
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
const every = Math.max(5, Number(document.body.dataset.refresh) || 30) * 1000;
setInterval(update, every);
document.addEventListener('visibilitychange', () => { if (!document.hidden) update(); });
"##;

/// A list that scrolls inside its panel once it is long.
fn scroll(label: &str) -> String {
    format!(
        "<div class=\"scroll\" tabindex=\"0\" role=\"region\" aria-label=\"{}\">",
        esc(label)
    )
}

/// `https://www.loc.gov/item/...` → `/item/...`: the host adds nothing in a
/// list where every address is LoC's.
fn short_urls(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("https://") {
        o.push_str(&rest[..i]);
        let url = &rest[i + "https://".len()..];
        let host_end = url.find('/').unwrap_or(url.len());
        let host = &url[..host_end];
        if host == "loc.gov" || host.ends_with(".loc.gov") {
            rest = &url[host_end..];
            // Every scan is under this on tile.loc.gov.
            if let Some(r) = rest.strip_prefix("/storage-services/service/ndnp/") {
                o.push_str("…/");
                rest = r;
            }
        } else {
            o.push_str("https://");
            rest = url;
        }
    }
    o.push_str(rest);
    o
}

/// An error cell: two lines at most, opening to the whole message (and
/// when, if the row shows only how long ago).
fn msg(e: &str, at: Option<DateTime<Utc>>) -> String {
    let whole = match at {
        Some(t) => format!("{}: {}", time(t), esc(e)),
        None => esc(e),
    };
    format!(
        "<td class=\"msg\">{}</td>",
        more(
            &format!("<span class=\"clamp\">{}</span>", esc(&short_urls(e))),
            &whole
        )
    )
}

fn next_table(o: &mut String, label: &str, rows: &[NextRow], with_fetched: bool) {
    if rows.is_empty() {
        o.push_str("<p class=\"muted\">None.</p>");
        return;
    }
    o.push_str(&scroll(label));
    o.push_str("<table><tr><th>#</th><th>doc_id</th><th>Lang</th><th>Group</th>");
    if with_fetched {
        o.push_str("<th>Scan</th>");
    }
    o.push_str("<th></th></tr>");
    for (i, r) in rows.iter().enumerate() {
        let _ = write!(
            o,
            "<tr><td class=\"r\">{}</td><td><code>{}</code></td><td>{}</td><td>{}</td>",
            i + 1,
            esc(&r.doc_id),
            esc(&r.lang),
            esc(r.group)
        );
        if with_fetched {
            let _ = write!(
                o,
                "<td>{}</td>",
                if r.fetched { "on pve-big" } else { "not yet" }
            );
        }
        let _ = write!(
            o,
            "<td>{}</td></tr>",
            if r.in_flight { "in flight" } else { "" }
        );
    }
    o.push_str("</table></div>");
}

fn sset_badge(x: &crate::view::SsetView) -> String {
    let b = match x.state.as_str() {
        "finished" => badge("good", "finished"),
        "running" => badge("good", "running"),
        other => badge("warning", other),
    };
    if x.stale {
        format!("{b} {}", badge("warning", "stale"))
    } else {
        b
    }
}

fn sset_eta(x: &crate::view::SsetView) -> String {
    if x.state == "finished" {
        return "done".to_owned();
    }
    x.status
        .eta_hours
        .map_or_else(|| "-".to_owned(), |h| span((h * 3600.0) as u64))
}

fn pipeline_panel(v: &crate::pipeline::PipelineView) -> String {
    let mut o =
        String::from("<section class=\"wide\" id=\"corpus-data-flow\"><h2>Corpus data flow</h2>");
    o.push_str(&badge(
        if v.stale { "warning" } else { "good" },
        if v.stale {
            "Some evidence old or unknown"
        } else {
            "observed"
        },
    ));
    o.push_str(
        "<p class=\"note\">Partial catalog; stage counts are separate from local worker totals, not whole-corpus totals. \
        <a href=\"/api/pipeline.json\">JSON</a></p>",
    );
    if let Some(s) = &v.snapshot {
        o.push_str(&scroll("Corpus stages"));
        o.push_str("<table class=\"pipeline-table\"><thead><tr><th>Stage / details</th><th class=\"r\">Count</th><th>State</th><th>Age</th></tr></thead><tbody>");
        for stage in &s.stages {
            let state = match stage.state {
                crate::pipeline::StageState::Observed => "observed",
                crate::pipeline::StageState::Unknown => "unknown",
                crate::pipeline::StageState::Blocked => "blocked",
            };
            let freshness = v.stage_freshness.iter().find(|f| f.id == stage.id);
            let _ = write!(
                o,
                "<tr><td><details data-refresh-key=\"stage-{}\"><summary>{}</summary><p>{}</p><p><code>{}</code></p>\
                 <p>Source evidence: {}</p></details></td><td class=\"r\">{}</td><td>{}{}</td><td>{}</td></tr>",
                esc(&stage.id),
                esc(&stage.label),
                esc(&stage.detail),
                esc(&stage.id),
                stage
                    .observed_at
                    .unwrap_or(s.observed_at)
                    .map_or_else(|| "unknown".into(), time),
                stage.count.map_or_else(|| "unknown".into(), int),
                state,
                if stage.state != crate::pipeline::StageState::Unknown
                    && freshness.is_none_or(|f| f.stale)
                {
                    " / stale"
                } else {
                    ""
                },
                freshness
                    .and_then(|f| f.age_secs)
                    .map_or_else(|| "unknown".into(), |age| span(age.max(0) as u64)),
            );
        }
        let _ = write!(
            o,
            "</tbody></table></div><details class=\"pipeline-meta\" data-refresh-key=\"corpus-metadata\"><summary>Coverage, provenance and issues ({})</summary>\
             <p>Cataloged runs: {}; cataloged run pages: {}. Cataloged artifact locations: {}; \
             verified: {}; missing: {}.</p>",
            s.issues.len(),
            int(s.runs.total),
            int(s.runs.pages),
            int(s.artifacts.locations),
            int(s.artifacts.verified),
            int(s.artifacts.missing)
        );
        if let Some(e) = &v.last_error {
            let _ = write!(o, "<p class=\"stale\">{}</p>", esc(e));
        }
        let _ = write!(
            o,
            "<p>Producer: {} (schema {}). Source evidence: {}. Generated: {}. \
             Retrieved: {}. Evidence age: {}.</p><p>Coverage: {}</p>",
            esc(&s.producer),
            s.schema_version,
            s.observed_at.map_or_else(|| "unknown".into(), time),
            time(s.generated_at),
            v.retrieved_at.map_or_else(|| "never".into(), time),
            v.age_secs
                .map_or_else(|| "unknown".into(), |n| span(n.max(0) as u64)),
            esc(&s.coverage.join(", "))
        );
        for issue in &s.issues {
            let _ = write!(o, "<p>{}: {}</p>", esc(&issue.code), esc(&issue.detail));
        }
        o.push_str("</details>");
    } else {
        o.push_str("<p>No valid catalog snapshot available.</p>");
        if let Some(e) = &v.last_error {
            let _ = write!(o, "<p class=\"stale\">{}</p>", esc(e));
        }
    }
    o.push_str("</section>");
    o
}

fn summary_stage<'a>(v: &'a View, id: &str) -> Option<&'a crate::pipeline::Stage> {
    let pipeline = &v.pipeline;
    let snapshot = pipeline.snapshot.as_ref()?;
    let stage = snapshot.stages.iter().find(|s| s.id == id)?;
    let freshness = pipeline.stage_freshness.iter().find(|s| s.id == id)?;
    (stage.state == crate::pipeline::StageState::Observed
        && stage.count.is_some()
        && !freshness.stale)
        .then_some(stage)
}

fn summary_count(v: &View, id: &str) -> String {
    summary_stage(v, id)
        .and_then(|s| s.count)
        .map_or_else(|| "not currently observed".into(), int)
}

fn summary_card(o: &mut String, title: &str, headline: &str, body: &str, note: &str) {
    let _ = write!(
        o,
        "<article class=\"ecosystem-card\"><h3>{}</h3><p class=\"headline\">{}</p>\
         <div>{body}</div><p class=\"note\">{note}</p></article>",
        esc(title),
        esc(headline),
    );
}

fn ecosystem_panel(v: &View) -> String {
    let now = v.generated_at;
    let mut o = String::from(
        "<section class=\"wide\" id=\"ecosystem\"><h2>Across the ecosystem</h2>\
         <p>Acquire originals, run OCR and comparisons, then validate and publish. \
         These lanes run independently: cached pages, worker completions and volunteer tasks \
         are different populations, not one progress bar.</p><div class=\"ecosystem-cards\">",
    );
    let f = &v.fetch;
    let fetch_time = f
        .status
        .as_ref()
        .and_then(|s| DateTime::parse_from_rfc3339(&s.updated_at).ok());
    let fetch_fresh = fetch_time
        .is_some_and(|t| (0..=900).contains(&(now - t.with_timezone(&Utc)).num_seconds()))
        && f.status_error.is_none()
        && f.plan_error.is_none();
    if fetch_fresh {
        let state = f.status.as_ref().map_or("unknown", |s| s.state.as_str());
        let keep = f.sets.iter().find(|s| s.name == "keep");
        let population = keep.map_or_else(
            || "Current cache population unavailable.".into(),
            |s| {
                format!(
                    "{} of {} planned keep-set originals cached ({}).",
                    int(s.fetched),
                    int(s.total),
                    bytes(s.bytes)
                )
            },
        );
        summary_card(&mut o, "Fetching originals",
            &format!("{} pages in the last hour", int(f.last_hour.pages)),
            &format!("{} {} {}",
                badge(if matches!(state, "running" | "done") { "good" } else { "warning" }, state),
                population,
                if f.status.as_ref().is_some_and(|s| s.error.is_some()) {
                    "Fetcher reports an error; see operational details."
                } else { "" }),
            &format!("{} source holds and {} failed attempts in the last hour. \
                Failures are attempts, not unique lost pages. Scan caching is not public-mirror verification.",
                int(f.holds_last_hour as u64), int(f.failures_last_hour as u64)));
    } else {
        summary_card(
            &mut o,
            "Fetching originals",
            "Current activity unknown",
            "Fetcher status or its plan is missing, old or invalid.",
            "Historical download details remain available below; no current rate is inferred.",
        );
    }
    let fresh_nodes: Vec<_> = v
        .nodes
        .iter()
        .filter(|n| {
            n.ok && n
                .last_ok
                .is_some_and(|at| now - at <= crate::view::NODE_FRESH)
        })
        .collect();
    if fresh_nodes.is_empty() {
        summary_card(
            &mut o,
            "Local OCR",
            "Current activity unknown",
            "No configured worker has fresh telemetry.",
            "Missing telemetry does not establish that the machines are idle.",
        );
    } else {
        let done: u64 = fresh_nodes.iter().map(|n| n.pages_per_hour).sum();
        let in_flight: usize = fresh_nodes.iter().map(|n| n.in_flight.len()).sum();
        let failed: u64 = fresh_nodes.iter().map(|n| n.last_hour.failed).sum();
        let abandoned: u64 = fresh_nodes.iter().map(|n| n.last_hour.abandoned).sum();
        let queue = v
            .azure
            .queues
            .as_ref()
            .filter(|q| !v.azure.queues_stale && q.error.is_none());
        let backlog = queue.and_then(|q| {
            Some(
                q.counts
                    .get("ocr-bulk")?
                    .saturating_add(*q.counts.get("ocr-priority")?),
            )
        });
        summary_card(
            &mut o,
            "Local OCR",
            &format!("{} pages completed in the last hour", int(done)),
            &format!(
                "{} pages in flight. Reporting workers: {}; without current status: {}. \
                Local queue: {}.",
                int(in_flight as u64),
                int(fresh_nodes.len() as u64),
                int((v.nodes.len() - fresh_nodes.len()) as u64),
                backlog.map_or_else(
                    || "unknown".into(),
                    |n| format!("about {} messages, including leased work", int(n))
                )
            ),
            &format!(
                "{} failed and {} abandoned attempts in the last hour may retry. \
                Completion means worker output, not accepted or searchable text.",
                int(failed),
                int(abandoned)
            ),
        );
    }
    let boinc_current = summary_stage(v, "boinc_unsent").is_some()
        && summary_stage(v, "boinc_in_progress").is_some();
    summary_card(&mut o, "Volunteer compute",
        &if boinc_current { format!("{} assigned result replicas", summary_count(v, "boinc_in_progress")) }
         else { "Current activity unknown".into() },
        &format!("{} unsent result replicas reported by the project. \
            Volunteer execution is a separate lane from local OCR.", summary_count(v, "boinc_unsent")),
        "Assigned does not mean actively computing. Replicas are not unique pages; \
         device eligibility, completion throughput and source cache age are not provided by this feed.");
    let mut research = String::new();
    let mut assigned = None::<u64>;
    for (app, name) in [
        ("quality", "Quality comparisons"),
        ("qwen", "Qwen crop readings"),
        ("iris", "Iris pilot readings"),
        ("gold", "Reference checks"),
        ("qwen_train", "Training work"),
    ] {
        let id = format!("boinc_usnm_{app}_in_progress");
        if let Some(count) = summary_stage(v, &id).and_then(|s| s.count) {
            assigned = Some(assigned.unwrap_or(0).saturating_add(count));
        }
        let _ = write!(
            research,
            "<li>{}: {} assigned replicas</li>",
            name,
            summary_count(v, &id)
        );
    }
    summary_card(
        &mut o,
        "Experiments and evaluation",
        &assigned.map_or_else(
            || "Current activity unknown".into(),
            |n| format!("{} assigned evaluation replicas", int(n)),
        ),
        &format!("<ul>{research}</ul>"),
        "These are subsets of volunteer work, not additional totals. Scheduler assignments show \
         activity, not experiment success, quality gains or approved rollout. Campaign readiness, \
         paired results and acceptance decisions are not connected to this dashboard.",
    );
    let ssets: Vec<_> = v.serial_set.iter().filter(|s| !s.stale).collect();
    if ssets.is_empty() {
        summary_card(
            &mut o,
            "Other source acquisition",
            "Current activity unknown",
            "Serial Set collection has no fresh report.",
            "This source extracts existing PDF text; it does not use the OCR queue.",
        );
    } else {
        let done: u64 = ssets.iter().map(|s| s.done).sum();
        let total: u64 = ssets.iter().map(|s| s.status.to_fetch).sum();
        let pages: u64 = ssets.iter().map(|s| s.status.pages).sum();
        let running = ssets.iter().filter(|s| s.state == "running").count();
        summary_card(
            &mut o,
            "Other source acquisition",
            &format!("{} Serial Set volumes processed (reported)", int(done)),
            &format!(
                "{} of {} planned volumes reported, containing {} extracted pages. \
                Collectors running: {}.",
                int(done),
                int(total),
                int(pages),
                int(running as u64)
            ),
            "Reports may overlap and include empty/incomplete volumes. \
             PDF text extraction is separate from OCR and search publication.",
        );
    }
    let snapshot = v.pipeline.snapshot.as_ref();
    let historical_stage = |id: &str| {
        snapshot.and_then(|s| {
            s.stages.iter().find(|stage| {
                stage.id == id
                    && stage.state == crate::pipeline::StageState::Observed
                    && stage.count.is_some()
            })
        })
    };
    let mut coverage = String::new();
    if let Some(s) = snapshot {
        if historical_stage("artifact_verified").is_some() {
            let _ = write!(
                coverage,
                "Cataloged artifact locations: {} ({} verified). ",
                int(s.artifacts.locations),
                int(s.artifacts.verified)
            );
        } else {
            coverage.push_str("Artifact inventory is not connected. ");
        }
        if historical_stage("run_history").is_some() {
            let _ = write!(
                coverage,
                "Recorded runs: {}; pages: {}. ",
                int(s.runs.total),
                int(s.runs.pages)
            );
        } else {
            coverage.push_str("Run history is not connected. ");
        }
        coverage.push_str("Imported evidence only, not corpus coverage.");
    } else {
        coverage.push_str("No valid catalog snapshot is loaded.");
    }
    let mut publishing = String::new();
    for (id, label) in [
        ("accepted", "Accepted"),
        ("imported", "Curated imports"),
        ("indexed", "Search inclusion"),
    ] {
        let _ = write!(publishing, "<li>{}: {}</li>", label, summary_count(v, id));
    }
    let publication_connected = ["accepted", "imported", "indexed"]
        .iter()
        .all(|id| summary_stage(v, id).and_then(|s| s.count).is_some());
    summary_card(
        &mut o,
        "Validation and publication",
        if publication_connected {
            "Publication counts reported"
        } else {
            "Coverage is incomplete"
        },
        &format!("<ul>{publishing}</ul>{coverage}"),
        "Execution counts do not establish OCR quality gains or publication. \
         Historical observations keep their original age when live counters refresh.",
    );
    o.push_str("</div>");
    if v.pipeline.last_error.is_some() {
        o.push_str(
            "<p class=\"note\">Some volunteer/catalog evidence is old, unknown or unreadable. \
            Source errors and individual ages are in operational details.</p>",
        );
    }
    if let Some(s) = snapshot {
        let _ = write!(
            o,
            "<p class=\"note\">Volunteer/catalog snapshot generated {} ({} ago). \
            Live-counter freshness is independent of historical evidence.</p>",
            time(s.generated_at),
            span((now - s.generated_at).num_seconds().max(0) as u64)
        );
    } else {
        o.push_str("<p class=\"note\">Volunteer/catalog snapshot unavailable. Local collection reports are independent.</p>");
    }
    o.push_str("</section>");
    o
}

pub fn render(v: &View) -> String {
    let now = v.generated_at;
    let mut o = String::with_capacity(64 * 1024);
    let _ = write!(
        o,
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <noscript><meta http-equiv=\"refresh\" content=\"{}\"></noscript>\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>USNewsMap processing</title><style>{STYLE}</style></head><body data-refresh=\"{}\">\
         <h1>USNewsMap processing</h1><div class=\"sub\">Source acquisition, local OCR, volunteer compute and evaluation. \
         Read-only; no page text. Made {}; refreshes every {}s. \
         JSON: <a href=\"/api/view.json\">/api/view.json</a>.</div><div class=\"grid dashboard\">",
        v.refresh_secs,
        v.refresh_secs,
        time(now),
        v.refresh_secs
    );
    o.push_str(&ecosystem_panel(v));
    o.push_str("<section class=\"wide\" id=\"operational-details\"><h2>Operational details</h2>\
        <details class=\"diagnostics\" data-refresh-key=\"operational-details\"><summary>\
        Show workers, queues, page lists, source evidence and reference diagrams</summary><div class=\"grid\">");

    // 0. At a glance.
    o.push_str("<section class=\"wide\"><h2>At a glance</h2><table><tr><th>What</th><th>State</th><th>Progress</th><th class=\"r\">Per hour</th><th class=\"r\">Done in</th></tr>");
    {
        let f = &v.fetch;
        let (state, per_hour) = match &f.status {
            Some(s) => (
                match s.state.as_str() {
                    "running" | "done" => badge("good", &s.state),
                    "waiting" | "budget" | "stopped" => badge("warning", &s.state),
                    other => badge("critical", other),
                },
                if f.last_hour.pages > 0 {
                    f.last_hour.pages_per_hour
                } else {
                    f.last_day.pages_per_hour
                },
            ),
            None => (badge("critical", "no status"), 0.0),
        };
        let keep = f.sets.iter().find(|s| s.name == "keep");
        let _ = write!(
            o,
            "<tr><td>Scan fetch, keep set</td><td>{state}</td><td>{}</td><td class=\"r\">{per_hour:.0} pages</td><td class=\"r\">{}</td></tr>",
            keep.map_or_else(
                || "-".to_owned(),
                |k| meter(k.fetched, k.total, &format!("{} of {} pages", int(k.fetched), int(k.total)))
            ),
            f.keep_eta_secs.map_or_else(|| "-".to_owned(), span)
        );
    }
    for n in &v.nodes {
        let state = match n.state {
            "working" => badge("good", "working"),
            "idle" => badge("warning", "idle"),
            other => badge("critical", other),
        };
        let _ = write!(
            o,
            "<tr><td>OCR {}</td><td>{state}</td><td>{} done in the last day; last page {}; up {}</td><td class=\"r\">{} pages</td><td class=\"r\">-</td></tr>",
            esc(&n.name),
            int(n.last_day.done),
            n.last_page_at.map_or_else(|| "-".to_owned(), |t| ago(now, t)),
            n.uptime_secs.map_or_else(|| "-".to_owned(), span),
            int(n.pages_per_hour)
        );
    }
    for x in &v.serial_set {
        let _ = write!(
            o,
            "<tr><td>Serial Set, {}</td><td>{}</td><td>{}</td><td class=\"r\">{:.1} volumes</td><td class=\"r\">{}</td></tr>",
            esc(&x.name),
            sset_badge(x),
            meter(x.done, x.status.to_fetch, &format!("{} of {} volumes", int(x.done), int(x.status.to_fetch))),
            x.status.volumes_per_hour,
            sset_eta(x)
        );
    }
    o.push_str("</table></section>");

    o.push_str(&pipeline_panel(&v.pipeline));

    // 1. Pool and hosts.
    o.push_str("<section><h2>Pool and hosts</h2>");
    if v.hosts.is_empty() {
        o.push_str("<p class=\"muted\">No host stats yet.</p>");
    }
    for h in &v.hosts {
        let _ = write!(o, "<h3>{}</h3>", esc(&h.name));
        if let Some(e) = &h.error {
            let _ = write!(o, "<p class=\"err\">{}</p>", esc(e));
        }
        if let Some(r) = h.collected_at {
            let stale = h.age_secs.unwrap_or(0) > 300;
            let _ = write!(
                o,
                "<table class=\"kv\"><tr><td>Stats</td><td>collected {} {}</td></tr>",
                ago(now, r),
                if stale {
                    badge("warning", "stale")
                } else {
                    String::new()
                }
            );
            let _ = write!(
                o,
                "<tr><td>CPU</td><td>{} of {} CPUs; load {}</td></tr>\
                 <tr><td>Memory</td><td>{}</td></tr><tr><td>Up</td><td>{}</td></tr></table>",
                meter(
                    (h.cpu_pct * 10.0) as u64,
                    1000,
                    &format!("{:.1}%", h.cpu_pct)
                ),
                h.cpus,
                esc(&h
                    .load
                    .iter()
                    .map(|l| format!("{l:.2}"))
                    .collect::<Vec<_>>()
                    .join(" / ")),
                meter(
                    h.mem_used,
                    h.mem_total,
                    &format!("{} of {}", bytes(h.mem_used), bytes(h.mem_total))
                ),
                span(h.uptime_secs)
            );
        }
        for p in &h.pools {
            let health = if p.health == "ONLINE" {
                badge("good", &p.health)
            } else {
                badge("critical", &p.health)
            };
            let _ = write!(
                o,
                "<table class=\"kv\"><tr><td>Pool <code>{}</code></td><td>{health} {}</td></tr>\
                 <tr><td>Capacity</td><td>{}</td></tr><tr><td>Fragmentation</td><td>{}%</td></tr>\
                 <tr><td>Last scan</td><td>{}</td></tr><tr><td>Errors</td><td>{}</td></tr></table>",
                esc(&p.name),
                esc(&p.status_x),
                meter(
                    p.alloc,
                    p.size,
                    &format!("{} of {} ({}%)", bytes(p.alloc), bytes(p.size), p.cap_pct)
                ),
                p.frag_pct,
                esc(&p.scan),
                esc(&p.errors)
            );
        }
        if !h.datasets.is_empty() {
            o.push_str("<table><tr><th>Dataset</th><th>Used</th><th class=\"r\">Logical</th><th class=\"r\">Ratio</th></tr>");
            for d in &h.datasets {
                let used = if d.quota > 0 {
                    meter(
                        d.used,
                        d.quota,
                        &format!("{} of {} quota", bytes(d.used), bytes(d.quota)),
                    )
                } else {
                    bytes(d.used)
                };
                let _ = write!(
                    o,
                    "<tr><td><code>{}</code></td><td>{used}</td><td class=\"r\">{}</td><td class=\"r\">{:.2}x</td></tr>",
                    esc(&d.name),
                    bytes(d.logicalused),
                    d.compressratio
                );
            }
            o.push_str("</table>");
        }
        if !h.guests.is_empty() {
            o.push_str("<h3>Guests</h3><table><tr><th>ID</th><th>Name</th><th>State</th><th class=\"r\">CPU</th><th>Memory</th><th class=\"r\">Up</th></tr>");
            for g in &h.guests {
                let st = if g.status == "running" {
                    badge("good", &g.status)
                } else {
                    badge("critical", &g.status)
                };
                let _ = write!(
                    o,
                    "<tr><td>{} {}</td><td>{}</td><td>{st}</td><td class=\"r\">{:.1}% of {}</td><td>{}</td><td class=\"r\">{}</td></tr>",
                    if g.kind == "lxc" { "CT" } else { "VM" },
                    g.vmid,
                    esc(&g.name),
                    g.cpu_pct,
                    g.cpus,
                    meter(g.mem, g.maxmem, &format!("{} of {}", bytes(g.mem), bytes(g.maxmem))),
                    span(g.uptime_secs)
                );
            }
            o.push_str("</table>");
        }
    }
    o.push_str("</section>");

    // 2. Downloads.
    let f = &v.fetch;
    o.push_str("<section><h2>Scan downloads</h2>");
    if let Some(e) = &f.status_error {
        let _ = write!(o, "<p class=\"err\">{}</p>", esc(e));
    }
    if let Some(e) = &f.plan_error {
        let _ = write!(o, "<p class=\"err\">Manifest: {}</p>", esc(e));
    }
    if let Some(s) = &f.status {
        let state = match s.state.as_str() {
            "running" | "done" => badge("good", &s.state),
            "waiting" | "budget" | "stopped" => badge("warning", &s.state),
            other => badge("critical", other),
        };
        let hold = match s.loc_hold_secs {
            Some(h) => format!(
                " {}",
                badge("warning", &format!("LoC hold, {} left", span(h)))
            ),
            None => String::new(),
        };
        let _ = write!(
            o,
            "<table class=\"kv\"><tr><td>Fetcher</td><td>{state}{hold} since {}, version {}</td></tr>\
             <tr><td>This run</td><td>{}</td></tr>\
             <tr><td>Last page</td><td><code>{}</code>, status written {}</td></tr>",
            time_str(&s.started_at),
            esc(&s.version),
            meter(
                s.finished,
                s.pages,
                &format!(
                    "{} of {} pages ({} already there, {} fetched, {} failed)",
                    int(s.finished),
                    int(s.pages),
                    int(s.already_there),
                    int(s.fetched),
                    int(s.failed_permanent + s.failed_transient)
                )
            ),
            esc(&s.last_doc_id),
            time_str(&s.updated_at)
        );
        if let Some(e) = &s.error {
            let _ = write!(
                o,
                "<tr><td>Error</td><td class=\"err\">{}</td></tr>",
                esc(e)
            );
        }
        let _ = write!(
            o,
            "<tr><td>Buffer</td><td>{}</td></tr></table>",
            meter(
                s.buffer_bytes,
                s.buffer_cap,
                &format!("{} of {} cap", bytes(s.buffer_bytes), bytes(s.buffer_cap))
            )
        );
    }
    let _ = write!(
        o,
        "<table><tr><th></th><th class=\"r\">Pages</th><th class=\"r\">Pages/h</th><th class=\"r\">MB/s</th><th class=\"r\">Data</th><th class=\"r\">LoC holds</th><th class=\"r\">Failures</th></tr>\
         <tr><td>Last hour</td><td class=\"r\">{}</td><td class=\"r\">{:.0}</td><td class=\"r\">{:.2}</td><td class=\"r\">{}</td><td class=\"r\">{}</td><td class=\"r\">{}</td></tr>\
         <tr><td>Last day</td><td class=\"r\">{}</td><td class=\"r\">{:.0}</td><td class=\"r\">{:.2}</td><td class=\"r\">{}</td><td class=\"r\">{}</td><td class=\"r\">{}</td></tr></table>",
        int(f.last_hour.pages), f.last_hour.pages_per_hour, f.last_hour.mb_per_s, bytes(f.last_hour.bytes), f.holds_last_hour, f.failures_last_hour,
        int(f.last_day.pages), f.last_day.pages_per_hour, f.last_day.mb_per_s, bytes(f.last_day.bytes), f.holds_last_day, f.failures_last_day,
    );
    if !f.holds_counted {
        o.push_str("<p class=\"note\">LoC holds are counted from the status file as it is read, so holds shorter than its 30-second rewrite can be missed.</p>");
    }
    if !f.sets.is_empty() {
        o.push_str(
            "<h3>By set</h3><table><tr><th>Set</th><th>Fetched</th><th class=\"r\">Data</th></tr>",
        );
        for s in &f.sets {
            let _ = write!(
                o,
                "<tr><td>{}</td><td>{}</td><td class=\"r\">{}</td></tr>",
                s.name,
                meter(
                    s.fetched,
                    s.total,
                    &format!("{} of {}", int(s.fetched), int(s.total))
                ),
                bytes(s.bytes)
            );
        }
        o.push_str("</table><h3>Keep set by group, in fetch order</h3><table><tr><th>Group</th><th>Fetched</th><th class=\"r\">Data</th><th class=\"r\">Done in</th></tr>");
        for g in &f.groups {
            let _ =
                write!(
                o,
                "<tr><td>{}{}</td><td>{}</td><td class=\"r\">{}</td><td class=\"r\">{}</td></tr>",
                esc(g.name),
                if g.current { " <span class=\"badge\">now</span>" } else { "" },
                meter(g.fetched, g.total, &format!("{} of {}", int(g.fetched), int(g.total))),
                bytes(g.bytes),
                match (g.eta_secs, g.total > 0 && g.fetched >= g.total) {
                    (_, true) => "done".to_owned(),
                    (Some(e), _) => span(e),
                    (None, _) => "-".to_owned(),
                }
            );
        }
        let _ = write!(
            o,
            "</table><p class=\"note\">Done in: at the last hour's rate (the last day's if the last hour had none). Keep set done in {}.</p>",
            f.keep_eta_secs.map_or_else(|| "-".to_owned(), span)
        );
    }
    let _ = write!(
        o,
        "<h3>Failures{}</h3><p>{} pages LoC can't serve (permanent failures). {} pages skipped for now because loc.gov answers 503 for their issue while serving others (<code>loc_item_error</code>); the fetcher asks again after a cool-off.</p>",
        if f.recent_failures.is_empty() {
            String::new()
        } else {
            format!(" ({} latest)", f.recent_failures.len())
        },
        int(f.dead_pages as u64),
        int(f.cooling_pages as u64)
    );
    if !f.recent_failures.is_empty() {
        o.push_str(&scroll("Latest failures"));
        o.push_str("<table><tr><th>When</th><th>doc_id</th><th>Kind</th><th>Error</th></tr>");
        for e in &f.recent_failures {
            let _ = write!(
                o,
                "<tr><td>{}</td><td><code>{}</code></td><td>{}</td>{}</tr>",
                ago(now, e.at),
                esc(&e.doc_id),
                esc(&e.kind),
                msg(&e.error, Some(e.at))
            );
        }
        o.push_str("</table></div>");
    }
    o.push_str("</section>");

    // 2b. Serial Set.
    o.push_str("<section><h2>Serial Set</h2>");
    if v.serial_set.is_empty() {
        o.push_str("<p class=\"muted\">No Serial Set status pushed yet.</p>");
    }
    for x in &v.serial_set {
        let st = &x.status;
        let time_opt = |t: Option<DateTime<Utc>>| t.map_or_else(|| "-".to_owned(), time);
        let _ = write!(
            o,
            "<h3>{} {}</h3><table class=\"kv\"><tr><td>Volumes</td><td>{}</td></tr>\
             <tr><td>Result</td><td>{} curated, {} empty, {} incomplete (some documents unread)</td></tr>\
             <tr><td>Pages curated</td><td>{}</td></tr>\
             <tr><td>Current volume</td><td><code>{}</code></td></tr>\
             <tr><td>Rate</td><td>{:.1} volumes and {:.0} documents an hour since {}</td></tr>\
             <tr><td>Left</td><td>{} volumes, about {} documents; done in {}</td></tr>\
             <tr><td>Status</td><td>made {}, received {}{}</td></tr></table>",
            esc(&x.name),
            sset_badge(x),
            meter(x.done, st.to_fetch, &format!("{} of {} volumes", int(x.done), int(st.to_fetch))),
            int(st.curated),
            int(st.empty),
            int(st.incomplete),
            int(st.pages),
            esc(st.current_volume.as_deref().unwrap_or("-")),
            st.volumes_per_hour,
            st.documents_per_hour,
            time_opt(st.started),
            int(st.left),
            int(st.documents_left_estimate),
            sset_eta(x),
            time_opt(st.at),
            ago(now, x.received),
            if x.stale { " (stale: pushed every 10 minutes)" } else { "" }
        );
    }
    o.push_str("</section>");

    // 3. Nodes.
    o.push_str("<section><h2>Nodes</h2>");
    if v.nodes.is_empty() {
        o.push_str("<p class=\"muted\">No nodes configured.</p>");
    }
    for n in &v.nodes {
        let state = if n.ok {
            badge("good", "reporting")
        } else {
            badge("critical", "no status")
        };
        let _ = write!(o, "<h3>{} {state}</h3>", esc(&n.name));
        if let Some(e) = &n.error {
            let _ = write!(
                o,
                "<p class=\"err\">{}: {}{}</p>",
                esc(&n.url),
                esc(e),
                n.last_ok
                    .map(|t| format!("; last answered {}", ago(now, t)))
                    .unwrap_or_default()
            );
        }
        if n.last_ok.is_none() {
            continue;
        }
        let routes = if n.routes.is_empty() {
            "none".to_owned()
        } else {
            n.routes
                .iter()
                .map(|(l, e)| format!("{} → {}", esc(l), esc(e)))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let cache = |o: &usnm_ocr_contract::status::Outcomes| {
            let all = o.from_cache + o.from_loc;
            if all == 0 {
                "-".to_owned()
            } else {
                format!(
                    "{} cache / {} LoC ({:.0}% cache)",
                    o.from_cache,
                    o.from_loc,
                    pct(o.from_cache, all)
                )
            }
        };
        let _ = write!(
            o,
            "<table class=\"kv\"><tr><td>Worker</td><td>{} (<code>{}</code>), {}, up {} since {}</td></tr>\
             <tr><td>Last page done</td><td>{}</td></tr>\
             <tr><td>Engine</td><td>{} {}; models {}</td></tr>\
             <tr><td>Routes</td><td>{routes}</td></tr>\
             <tr><td>Concurrency</td><td>{}; {} in flight</td></tr>\
             <tr><td>Scans</td><td>{}</td></tr>{}</table>",
            esc(&n.version),
            esc(&n.node_id),
            n.state,
            n.uptime_secs.map_or_else(|| "-".to_owned(), span),
            n.started_at.map_or_else(|| "-".to_owned(), time),
            n.last_page_at.map_or_else(|| "none yet".to_owned(), |t| format!("{}, {}", time(t), ago(now, t))),
            esc(&n.engine),
            esc(&n.engine_version),
            esc(&n.models.join(", ")),
            n.concurrency,
            n.in_flight.len(),
            n.scan_cache_url.as_deref().map_or_else(
                || "from LoC only".to_owned(),
                |u| format!("scan server {} first", esc(u))
            ),
            n.loc_hold_secs
                .map(|h| format!(
                    "<tr><td>LoC</td><td>{}</td></tr>",
                    badge("warning", &format!("hold, {} left", span(h)))
                ))
                .unwrap_or_default()
        );
        let _ = write!(
            o,
            "<table><tr><th></th><th class=\"r\">Done</th><th class=\"r\">Failed</th><th class=\"r\">Abandoned</th><th class=\"r\">Dead-lettered</th><th>Scans</th></tr>\
             <tr><td>Last hour</td><td class=\"r\">{}</td><td class=\"r\">{}</td><td class=\"r\">{}</td><td class=\"r\">{}</td><td>{}</td></tr>\
             <tr><td>Last day</td><td class=\"r\">{}</td><td class=\"r\">{}</td><td class=\"r\">{}</td><td class=\"r\">{}</td><td>{}</td></tr></table>\
             <p>{} pages an hour ({:.2} a minute) over the last hour.</p>",
            n.last_hour.done, n.last_hour.failed, n.last_hour.abandoned, n.last_hour.dead_lettered, cache(&n.last_hour),
            n.last_day.done, n.last_day.failed, n.last_day.abandoned, n.last_day.dead_lettered, cache(&n.last_day),
            int(n.pages_per_hour),
            n.pages_per_min
        );
        if !n.in_flight.is_empty() {
            let _ = write!(
                o,
                "<h3>In flight on {} ({})</h3>{}",
                esc(&n.name),
                n.in_flight.len(),
                scroll(&format!("In flight on {}", n.name))
            );
            o.push_str("<table><tr><th>doc_id</th><th>Lang</th><th>Queue</th><th>Stage</th><th>Started</th></tr>");
            for p in &n.in_flight {
                let _ = write!(
                    o,
                    "<tr><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                    esc(&p.doc_id),
                    esc(&p.language),
                    esc(&p.queue),
                    esc(&p.stage),
                    ago(now, p.started_at)
                );
            }
            o.push_str("</table></div>");
        }
    }
    o.push_str("</section>");

    // 4. What's next.
    let x = &v.next;
    o.push_str("<section><h2>What's next</h2>");
    let _ = write!(
        o,
        "<h3>To OCR ({} of {} left): keep set in fetch order, not yet OCRed</h3>",
        x.to_ocr.len(),
        int(x.keep_left_to_ocr)
    );
    next_table(&mut o, "To OCR", &x.to_ocr, true);
    o.push_str("<p class=\"note\">The queues decide what a node takes; this list is the keep set in the fetcher's order. \
                A page counts as OCRed once a node's journal says so.</p>");
    let _ = write!(o, "<h3>To fetch (next {})</h3>", x.to_fetch.len());
    next_table(&mut o, "To fetch", &x.to_fetch, false);
    o.push_str("<h3>Seed plan</h3>");
    match &x.seed_plan {
        Some(p) => {
            o.push_str("<table class=\"kv\">");
            for (k, val) in p {
                let _ = write!(o, "<tr><td>{}</td><td>{}</td></tr>", esc(k), esc(val));
            }
            o.push_str("</table>");
        }
        None => o.push_str("<p class=\"note\">No seed plan file. The cloud seeder's settings (USNM_OCR_SEED_*) are on the ocr-sync job in Azure.</p>"),
    }
    o.push_str("</section>");

    // 5. What's been OCRed.
    let d = &v.ocred;
    o.push_str("<section><h2>What's been OCRed</h2>");
    let _ = write!(
        o,
        "<p>{} pages OCRed by the nodes, as their journals report.</p>",
        int(d.pages)
    );
    let pairs = |o: &mut String, title: &str, rows: &[(String, u64)]| {
        if rows.is_empty() {
            return;
        }
        let _ = write!(o, "<h3>{title}</h3><table>");
        for (k, n) in rows {
            let _ = write!(
                o,
                "<tr><td>{}</td><td class=\"r\">{}</td></tr>",
                if k.is_empty() {
                    "-".to_owned()
                } else {
                    clip(k)
                },
                int(*n)
            );
        }
        o.push_str("</table>");
    };
    pairs(&mut o, "By language", &d.by_language);
    pairs(&mut o, "By engine", &d.by_engine);
    for l in &d.latest {
        let _ = write!(o, "<h3>Latest on {} ({})</h3>", esc(&l.node), l.rows.len());
        if l.rows.is_empty() {
            o.push_str("<p class=\"muted\">None yet.</p>");
            continue;
        }
        o.push_str(&scroll(&format!("Latest on {}", l.node)));
        o.push_str("<table><tr><th>doc_id</th><th>Lang</th><th>Engine</th><th>Outcome</th><th>Uploaded</th><th>Scan</th><th class=\"r\">Secs</th><th>Finished</th></tr>");
        for c in &l.rows {
            let _ = write!(
                o,
                "<tr><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td class=\"r\">{:.0}</td><td>{}</td></tr>",
                esc(&c.doc_id),
                esc(&c.language),
                clip(&c.engine),
                match c.outcome.as_str() {
                    "done" => badge("good", "done"),
                    other => badge(
                        "warning",
                        &match &c.error_kind {
                            Some(k) => format!("{other} ({k})"),
                            None => other.to_owned(),
                        }
                    ),
                },
                if c.uploaded { "yes" } else { "no" },
                esc(&c.scan),
                c.secs,
                time(c.finished_at)
            );
        }
        o.push_str("</table></div>");
    }
    let _ = write!(
        o,
        "<h3>Imports</h3><p class=\"note\">{}",
        esc(v.azure.imports)
    );
    if let Some(u) = &d.workbook_url {
        let _ = write!(o, " See the <a href=\"{}\">OCR nodes workbook</a>.", esc(u));
    }
    o.push_str("</p></section>");

    // 6. Queues.
    o.push_str("<section><h2>Azure queues and dead letters</h2>");
    match (&v.azure.queues, &v.azure.queues_from) {
        (Some(q), Some(from)) => {
            let _ = write!(
                o,
                "<p>As {} saw them {}{}.</p>",
                esc(from),
                ago(now, q.checked_at),
                if v.azure.queues_stale {
                    format!(" {}", badge("warning", "stale"))
                } else {
                    String::new()
                }
            );
            if let Some(e) = &q.error {
                let _ = write!(o, "<p class=\"err\">{}</p>", esc(e));
            }
            o.push_str("<table><tr><th>Queue</th><th class=\"r\">Messages</th></tr>");
            for (name, n) in &q.counts {
                let _ = write!(
                    o,
                    "<tr><td><code>{}</code></td><td class=\"r\">{}</td></tr>",
                    esc(name),
                    int(*n)
                );
            }
            o.push_str("</table>");
            if q.dead_letters.is_empty() {
                o.push_str("<p class=\"muted\">No dead letters visible.</p>");
            } else {
                let _ = write!(
                    o,
                    "<h3>Dead letters, oldest first ({})</h3>{}",
                    q.dead_letters.len(),
                    scroll("Dead letters")
                );
                o.push_str("<table><tr><th>doc_id</th><th>From</th><th>Cause</th><th class=\"r\">Received</th><th>Node</th><th>When</th><th>Error</th></tr>");
                for d in &q.dead_letters {
                    let cause = match &d.kind {
                        Some(k) => format!("{} ({k})", d.cause),
                        None => d.cause.clone(),
                    };
                    let _ = write!(
                        o,
                        "<tr><td><code>{}</code></td><td>{}</td><td>{}</td><td class=\"r\">{}</td><td>{}</td><td>{}</td>{}</tr>",
                        if d.doc_id.is_empty() { "-".to_owned() } else { esc(&d.doc_id) },
                        esc(&d.from),
                        esc(&cause),
                        d.dequeue_count,
                        esc(&d.node),
                        time_str(&d.at),
                        msg(&d.error, None)
                    );
                }
                o.push_str("</table></div>");
            }
        }
        _ => o.push_str("<p class=\"muted\">No node has reported the queues yet.</p>"),
    }
    let _ = write!(
        o,
        "<p class=\"note\">{}</p></section>",
        esc(v.azure.queues_note)
    );

    o.push_str(&crate::diagrams::section(v));
    let _ = write!(
        o,
        "</div></details></section></div><script>{SCRIPT}</script></body></html>"
    );
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corpus_panel_escapes_metadata_and_keeps_catalog_counts_separate() {
        let now = Utc::now();
        let mut source = crate::pipeline::tests::fixture(now);
        source["coverage"] = serde_json::json!(["<coverage>"]);
        source["stages"][0]["label"] = serde_json::json!("<stage>");
        source["stages"][0]["detail"] = serde_json::json!("\"details\" & <text>");
        source["issues"][0]["detail"] = serde_json::json!("<script>alert(1)</script>");
        let snapshot = crate::pipeline::parse(&serde_json::to_vec(&source).unwrap(), now).unwrap();
        let v = crate::pipeline::PipelineView {
            retrieved_at: Some(now),
            last_error: Some("<error>".into()),
            age_secs: Some(901),
            stale: true,
            stage_freshness: Vec::new(),
            snapshot: Some(snapshot),
        };
        let html = pipeline_panel(&v);
        assert!(html.contains("Corpus data flow"));
        assert!(html.contains("<section class=\"wide\" id=\"corpus-data-flow\">"));
        assert!(html.contains("aria-label=\"Corpus stages\""));
        assert!(html.contains("<table class=\"pipeline-table\">"));
        assert_eq!(html.matches("data-refresh-key=\"stage-").count(), 2);
        assert!(!html.contains("colspan=\"3\""));
        assert!(html.contains("data-refresh-key=\"corpus-metadata\"><summary>"));
        assert!(html.contains("Some evidence old or unknown"));
        assert!(html.contains("separate from local worker totals"));
        for s in [
            "&lt;coverage&gt;",
            "&lt;stage&gt;",
            "&lt;error&gt;",
            "&lt;script&gt;",
        ] {
            assert!(html.contains(s));
        }
        assert!(!html.contains("<script>"));
        assert!(html.contains("1,234"));
        assert!(html.contains("unknown"));
        assert!(html.contains("Source evidence: <time"));
        assert!(html.contains("Cataloged runs: 2; cataloged run pages: 100"));
        assert!(pipeline_panel(&crate::pipeline::PipelineView::default())
            .contains("No valid catalog snapshot"));
    }

    #[test]
    fn blocked_stages_keep_their_stale_warning_and_stable_disclosure_key() {
        let now = Utc::now();
        let mut source = crate::pipeline::tests::fixture(now);
        source["stages"][0]["state"] = serde_json::json!("blocked");
        let snapshot = crate::pipeline::parse(&serde_json::to_vec(&source).unwrap(), now).unwrap();
        let v = crate::pipeline::PipelineView {
            snapshot: Some(snapshot),
            ..Default::default()
        };
        let html = pipeline_panel(&v);
        assert!(html.contains("blocked / stale"));
        assert!(html.contains("data-refresh-key=\"stage-scan\""));
        assert!(html.contains("data-refresh-key=\"corpus-metadata\""));
        assert!(SCRIPT.contains("d.dataset.refreshKey ||"));
    }

    fn summary_view() -> View {
        let dir = tempfile::tempdir().unwrap();
        crate::view::build(
            &mut crate::fetch::Fetch::new(dir.path().to_owned(), None),
            &crate::nodes::Nodes::open(
                dir.path().join("state"),
                &[("worker-sentinel".into(), "http://127.0.0.1:1".into())],
            )
            .unwrap(),
            &crate::hosts::Hosts::new(Vec::new()),
            &crate::sset::Ssets::default(),
            None,
            &crate::view::Settings::default(),
            Utc::now(),
        )
    }

    #[test]
    fn ecosystem_summaries_do_not_expose_page_or_batch_identifiers() {
        let mut v = summary_view();
        v.fetch.status = Some(crate::fetch::FetchStatus {
            updated_at: v.generated_at.to_rfc3339(),
            state: "running".into(),
            last_doc_id: "private-page-sentinel".into(),
            manifest: "private-batch-sentinel".into(),
            ..Default::default()
        });
        v.fetch.status_error = None;
        v.fetch.plan_error = None;
        v.fetch.last_hour.pages = 1574;
        let summary = ecosystem_panel(&v);
        assert!(summary.contains("1,574 pages in the last hour"));
        assert!(summary.contains("Volunteer compute"));
        assert!(summary.contains("Experiments and evaluation"));
        assert!(summary.contains("Validation and publication"));
        assert!(summary.contains("Current activity unknown"));
        assert!(!summary.contains("private-page-sentinel"));
        assert!(!summary.contains("private-batch-sentinel"));
        assert!(!summary.contains("STALE"));
        let html = render(&v);
        assert!(html.find("Across the ecosystem") < html.find("Operational details"));
        assert!(html
            .contains("<details class=\"diagnostics\" data-refresh-key=\"operational-details\">"));
        assert!(SCRIPT.contains("section:not(#operational-details)"));
        assert!(SCRIPT.contains("'.diagnostics > .grid'"));
        assert!(!SCRIPT.contains(".grid > section"));
    }

    #[test]
    fn summary_uses_fresh_volunteer_counts_without_hiding_unknown_acceptance() {
        let mut v = summary_view();
        let mut source = crate::pipeline::tests::fixture(v.generated_at);
        source["stages"] = serde_json::json!([
            {"id":"boinc_unsent","label":"Unsent","count":12345,"state":"observed","detail":""},
            {"id":"boinc_in_progress","label":"Assigned","count":6789,"state":"observed","detail":""},
            {"id":"boinc_usnm_iris_in_progress","label":"Iris","count":2345,"state":"observed","detail":""}
        ]);
        let snapshot =
            crate::pipeline::parse(&serde_json::to_vec(&source).unwrap(), v.generated_at).unwrap();
        v.pipeline.stage_freshness = snapshot
            .stages
            .iter()
            .map(|s| crate::pipeline::StageFreshness {
                id: s.id.clone(),
                age_secs: Some(5),
                stale: false,
            })
            .collect();
        v.pipeline.snapshot = Some(snapshot);
        // Historical or unknown stages may mark the evidence bundle stale without hiding fresh lanes.
        v.pipeline.stale = true;
        let html = ecosystem_panel(&v);
        assert!(html.contains("6,789 assigned result replicas"));
        assert!(html.contains("12,345 unsent"));
        assert!(html.contains("Iris pilot readings: 2,345 assigned"));
        assert!(html.contains("Accepted: not currently observed"));
        v.pipeline
            .stage_freshness
            .iter_mut()
            .for_each(|s| s.stale = true);
        let stale = ecosystem_panel(&v);
        assert!(!stale.contains("6,789 assigned result replicas"));
        assert!(!stale.contains("12,345 unsent"));
        assert!(stale.contains("Current activity unknown"));
    }

    #[test]
    fn summaries_exclude_old_or_failed_reports_and_keep_queue_units() {
        let mut v = summary_view();
        let node = |done, at, ok| {
            let mut n = summary_view().nodes.remove(0);
            n.ok = ok;
            n.last_ok = Some(at);
            n.pages_per_hour = done;
            n.last_hour.failed = 2;
            n
        };
        v.nodes = vec![
            node(10, v.generated_at, true),
            node(20, v.generated_at, true),
            node(10000, v.generated_at - chrono::Duration::minutes(4), true),
            node(10000, v.generated_at, false),
        ];
        v.azure.queues_stale = false;
        v.azure.queues = Some(usnm_ocr_contract::status::QueueStats {
            counts: [
                ("ocr-bulk".into(), 607),
                ("ocr-priority".into(), 3),
                ("ocr-deadletter".into(), 999),
            ]
            .into(),
            ..Default::default()
        });
        let summary = ecosystem_panel(&v);
        assert!(summary.contains("30 pages completed in the last hour"));
        assert!(summary.contains("Reporting workers: 2; without current status: 2"));
        assert!(summary.contains("4 failed and 0 abandoned attempts"));
        assert!(summary.contains("about 610 messages, including leased work"));
        assert!(!summary.contains("worker-sentinel"));
        v.azure.queues_stale = true;
        assert!(ecosystem_panel(&v).contains("Local queue: unknown"));
        v.fetch.status = Some(crate::fetch::FetchStatus {
            state: "running".into(),
            updated_at: (v.generated_at - chrono::Duration::minutes(16)).to_rfc3339(),
            ..Default::default()
        });
        v.fetch.status_error = None;
        v.fetch.plan_error = None;
        v.fetch.last_hour.pages = 123456;
        assert!(!ecosystem_panel(&v).contains("123,456 pages in the last hour"));
        v.fetch.status.as_mut().unwrap().updated_at = v.generated_at.to_rfc3339();
        v.fetch.plan_error = Some("bad plan".into());
        assert!(!ecosystem_panel(&v).contains("123,456 pages in the last hour"));
    }

    #[test]
    fn malformed_or_missing_snapshot_suppresses_retained_live_counts() {
        let mut v = summary_view();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapshot.json");
        let mut source = crate::pipeline::tests::fixture(v.generated_at);
        source["stages"] = serde_json::json!([
            {"id":"boinc_unsent","label":"Unsent","count":12345,"state":"observed","detail":""},
            {"id":"boinc_in_progress","label":"Assigned","count":6789,"state":"observed","detail":""}
        ]);
        std::fs::write(&path, serde_json::to_vec(&source).unwrap()).unwrap();
        let mut pipeline = crate::pipeline::Pipeline::new(Some(path.clone()), 900);
        pipeline.poll(v.generated_at);
        v.pipeline = pipeline.view(v.generated_at);
        assert!(ecosystem_panel(&v).contains("6,789 assigned result replicas"));
        v.pipeline = pipeline.view(v.generated_at + chrono::Duration::minutes(16));
        assert!(!ecosystem_panel(&v).contains("6,789 assigned result replicas"));
        std::fs::write(&path, b"not valid JSON").unwrap();
        pipeline.poll(v.generated_at);
        v.pipeline = pipeline.view(v.generated_at);
        assert!(v.pipeline.snapshot.is_some());
        assert!(!ecosystem_panel(&v).contains("6,789 assigned result replicas"));
        assert!(ecosystem_panel(&v).contains("old, unknown or unreadable"));
        std::fs::remove_file(path).unwrap();
        pipeline.poll(v.generated_at);
        v.pipeline = pipeline.view(v.generated_at);
        assert!(!ecosystem_panel(&v).contains("6,789 assigned result replicas"));
    }

    #[test]
    fn missing_catalog_and_runs_only_catalog_are_not_empty_inventories() {
        let mut v = summary_view();
        let mut source = crate::pipeline::tests::fixture(v.generated_at);
        source["stages"] = serde_json::json!([
            {"id":"artifact_verified","label":"Artifacts","count":null,"state":"unknown","detail":""},
            {"id":"run_history","label":"Runs","count":null,"state":"unknown","detail":""}
        ]);
        source["artifacts"] = serde_json::json!({"locations":0,"verified":0,"missing":0});
        source["runs"] = serde_json::json!({"total":0,"pages":0});
        v.pipeline.snapshot = Some(
            crate::pipeline::parse(&serde_json::to_vec(&source).unwrap(), v.generated_at).unwrap(),
        );
        let unknown = ecosystem_panel(&v);
        assert!(unknown.contains("Artifact inventory is not connected"));
        assert!(unknown.contains("Run history is not connected"));
        assert!(!unknown.contains("Recorded runs: 0"));
        assert!(!unknown.contains("artifact locations: 0"));
        source["runs"] = serde_json::json!({"total":5,"pages":3});
        source["stages"][1]["state"] = "observed".into();
        source["stages"][1]["count"] = 3.into();
        source["stages"][1]["observed_at"] = (v.generated_at - chrono::Duration::days(1))
            .to_rfc3339()
            .into();
        v.pipeline.snapshot = Some(
            crate::pipeline::parse(&serde_json::to_vec(&source).unwrap(), v.generated_at).unwrap(),
        );
        let runs = ecosystem_panel(&v);
        assert!(runs.contains("Recorded runs: 5; pages: 3"));
        assert!(runs.contains("Artifact inventory is not connected"));
        assert!(runs.contains("Imported evidence only, not corpus coverage"));
    }

    #[test]
    fn formats() {
        assert_eq!(
            esc("<a href=\"x\">'&'</a>"),
            "&lt;a href=&quot;x&quot;&gt;&#39;&amp;&#39;&lt;/a&gt;"
        );
        assert_eq!(bytes(999), "999 B");
        assert_eq!(bytes(1_215_076_923), "1.2 GB");
        assert_eq!(bytes(3_298_534_883_328), "3.3 TB");
        assert_eq!(span(691_999), "8d 0h");
        assert_eq!(span(3_700), "1h 1m");
        assert_eq!(span(42), "42s");
        assert_eq!(int(224_848), "224,848");
        assert_eq!(int(5), "5");
        assert_eq!(pct(1, 0), 0.0);
    }

    #[test]
    fn loc_addresses_lose_their_host() {
        assert_eq!(
            short_urls("https://www.loc.gov/item/sn1/1904-09-08/ed-1/?fo=json returned HTTP 503"),
            "/item/sn1/1904-09-08/ed-1/?fo=json returned HTTP 503"
        );
        assert_eq!(
            short_urls(
                "https://tile.loc.gov/storage-services/service/ndnp/ga/batch_x/data/0123.jp2: 404"
            ),
            "…/ga/batch_x/data/0123.jp2: 404"
        );
        assert_eq!(
            short_urls("https://example.com/x and https://loc.gov"),
            "https://example.com/x and "
        );
        assert_eq!(
            msg("a <b> https://www.loc.gov/item/x/", None),
            "<td class=\"msg\"><details class=\"more\"><summary><span class=\"clamp\">a &lt;b&gt; /item/x/</span></summary>\
             <div>a &lt;b&gt; https://www.loc.gov/item/x/</div></details></td>"
        );
    }
}
