//! Operational-detail panels in their existing page order.

use super::ecosystem::pipeline_panel;
use super::fmt::*;
use crate::view::{NextRow, View};
use chrono::{DateTime, Utc};
use std::borrow::Cow;
use std::fmt::Write as _;

fn next_table(o: &mut String, label: &str, rows: &[NextRow]) {
    if rows.is_empty() {
        o.push_str("<p class=\"muted\">None.</p>");
        return;
    }
    o.push_str(&scroll(label));
    o.push_str("<table><tr><th>#</th><th>doc_id</th><th>Lang</th><th>Manifest</th></tr>");
    for (i, r) in rows.iter().enumerate() {
        let _ = write!(
            o,
            "<tr><td class=\"r\">{}</td><td><code>{}</code></td><td>{}</td><td><code>{}</code></td></tr>",
            i + 1,
            esc(&r.doc_id),
            esc(&r.lang),
            esc(&r.manifest)
        );
    }
    o.push_str("</table></div>");
}

pub(super) fn at_a_glance(v: &View, o: &mut String) {
    let now = v.generated_at;
    o.push_str("<section class=\"wide\"><h2>At a glance</h2><table><tr><th>What</th><th>State</th><th>Progress</th><th class=\"r\">Per hour</th><th class=\"r\">Done in</th></tr>");
    {
        let f = &v.fetch;
        let per_hour = if f.last_hour.pages > 0 {
            f.last_hour.pages_per_hour
        } else {
            f.last_day.pages_per_hour
        };
        let run = f.status.as_ref().map_or_else(
            || "-".to_owned(),
            |s| {
                format!(
                    "this run: {} of {} pages of <code>{}</code>",
                    int(s.finished),
                    int(s.pages),
                    esc(&manifest_name(&s.manifest))
                )
            },
        );
        let _ = write!(
            o,
            "<tr><td>Scan fetch</td><td>{}</td><td>{run}; {} pages fetched across all runs</td><td class=\"r\">{per_hour:.0} pages</td><td class=\"r\">-</td></tr>",
            fetch_badge(f),
            int(f.total.pages)
        );
        if let Some(e) = f.queue_error.as_deref().filter(|_| f.queue_configured) {
            let _ = write!(
                o,
                "<tr><td>Fetch queue</td><td>{}</td><td class=\"err\">{}</td><td class=\"r\">-</td><td class=\"r\">-</td></tr>",
                badge("critical", "unknown"),
                esc(e)
            );
        } else if f.queue_configured {
            let qs = &f.queue_summary;
            let _ = write!(
                o,
                "<tr><td>Fetch queue</td><td>{}</td><td>{}</td><td class=\"r\">-</td><td class=\"r\">-</td></tr>",
                if qs.queued + qs.fetching > 0 {
                    badge("good", &format!("{} waiting", int((qs.queued + qs.fetching) as u64)))
                } else {
                    badge("good", "empty")
                },
                meter(
                    qs.pages_fetched,
                    qs.pages,
                    &format!(
                        "{} of {} pages in {} manifests; {} left",
                        int(qs.pages_fetched),
                        int(qs.pages),
                        int(qs.manifests as u64),
                        int(qs.pages_left)
                    )
                )
            );
        }
        if let Some(k) = f.sets.iter().find(|s| s.name == "keep") {
            let _ = write!(
                o,
                "<tr><td>Keep set, {}</td><td>{}</td><td>{}</td><td class=\"r\">-</td><td class=\"r\">{}</td></tr>",
                esc(&f.plan.as_deref().map(manifest_name).unwrap_or_default()),
                if k.fetched >= k.total { badge("good", "done") } else { badge("good", "planned") },
                meter(k.fetched, k.total, &format!("{} of {} pages", int(k.fetched), int(k.total))),
                f.keep_eta_secs.map_or_else(|| "-".to_owned(), span)
            );
        }
        if let Some(b) = &f.bulk.value {
            let _ = write!(
                o,
                "<tr><td>Bulk OCR archives</td><td>{}</td><td>{}</td><td class=\"r\">{}/s</td><td class=\"r\">{}</td></tr>",
                state_badge(&b.state),
                meter(b.done, b.archives, &format!("{} of {} archives", int(b.done), int(b.archives))),
                bytes(b.bytes_per_sec.max(0.0) as u64),
                bulk_eta(b).map_or_else(|| "-".to_owned(), span)
            );
        }
    }
    for n in &v.nodes {
        let state = match n.state {
            "working" => badge("good", "working"),
            "idle" | "retired" => badge("warning", n.state),
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
}

pub(super) fn pool_and_hosts(v: &View, o: &mut String) {
    let now = v.generated_at;
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
                    "<tr><td>{}</td><td>{}</td><td>{st}</td><td class=\"r\">{:.1}% of {}</td><td>{}</td><td class=\"r\">{}</td></tr>",
                    match (v.public, g.kind == "lxc") {
                        (true, true) => Cow::Borrowed("container"),
                        (true, false) => Cow::Borrowed("VM"),
                        (false, true) => Cow::Owned(format!("CT {}", g.vmid)),
                        (false, false) => Cow::Owned(format!("VM {}", g.vmid)),
                    },
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
}

pub(super) fn downloads(v: &View, o: &mut String) {
    let now = v.generated_at;
    let f = &v.fetch;
    o.push_str("<section><h2>Scan downloads</h2>");
    if let Some(e) = &f.status_error {
        let _ = write!(o, "<p class=\"err\">{}</p>", esc(e));
    }
    if let Some(e) = &f.plan_error {
        let _ = write!(o, "<p class=\"err\">Plan: {}</p>", esc(e));
    }
    if let Some(e) = &f.index_error {
        let _ = write!(o, "<p class=\"err\">Index: {}</p>", esc(e));
    }
    let _ = write!(
        o,
        "<p>{} {} pages fetched across all runs ({}), from the fetch index.</p>",
        fetch_badge(f),
        int(f.total.pages),
        bytes(f.total.bytes)
    );
    if let Some(s) = &f.status {
        let state = state_badge(&s.state);
        let hold = match s.loc_hold_secs {
            Some(h) => format!(
                " {}",
                badge("warning", &format!("LoC hold, {} left", span(h)))
            ),
            None => String::new(),
        };
        let _ = write!(
            o,
            "<table class=\"kv\"><tr><td>Last run</td><td>{state}{hold} since {}, version {}</td></tr>\
             <tr><td>Manifest</td><td><code>{}</code></td></tr>\
             <tr><td>This run</td><td>{}</td></tr>\
             <tr><td>Routes</td><td>{}</td></tr>\
             <tr><td>Last page</td><td><code>{}</code>, status written {}</td></tr>",
            time_str(&s.started_at),
            esc(&s.version),
            esc(&manifest_name(&s.manifest)),
            meter(
                s.finished,
                s.pages,
                &format!(
                    "{} of {} pages ({} already there, {} fetched, {} failed{})",
                    int(s.finished),
                    int(s.pages),
                    int(s.already_there),
                    int(s.fetched),
                    int(s.failed_permanent + s.failed_transient),
                    if s.no_loc || s.deferred_needs_loc > 0 {
                        format!(", {} left for LoC", int(s.deferred_needs_loc))
                    } else {
                        String::new()
                    }
                )
            ),
            format_args!(
                "{} JP2 ({} from mirrors, {} from LoC's bucket), {} through the item API",
                int(s.fetched_jp2),
                int(s.fetched_mirror),
                int(s.fetched_bucket),
                int(s.fetched_item_api)
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
        let routes: Vec<(&str, &crate::fetch::RouteCounts)> = s
            .bucket
            .iter()
            .map(|b| ("LoC bucket", b))
            .chain(s.mirrors.iter().map(|(n, m)| (n.as_str(), m)))
            .collect();
        if !routes.is_empty() {
            o.push_str("<h3>Routes this run</h3><table><tr><th>Route</th><th class=\"r\">Requests</th><th class=\"r\">JP2s</th><th class=\"r\">Misses</th><th class=\"r\">md5 mismatches</th><th class=\"r\">Holds</th><th>Held</th></tr>");
            for (name, r) in routes {
                let _ = write!(
                    o,
                    "<tr><td>{}</td><td class=\"r\">{}</td><td class=\"r\">{}</td><td class=\"r\">{}</td><td class=\"r\">{}</td><td class=\"r\">{}</td><td>{}</td></tr>",
                    esc(name),
                    int(r.requests),
                    int(r.taken()),
                    int(r.misses),
                    int(r.md5_mismatches),
                    int(r.holds),
                    r.held_secs.map_or_else(|| "-".to_owned(), |h| format!("{} left", span(h)))
                );
            }
            o.push_str("</table>");
        }
    }
    if let Some(p) = &f.pacer.value {
        let until =
            DateTime::<Utc>::from_timestamp_millis(p.hold_until_ms as i64).filter(|t| *t > now);
        let _ = write!(
            o,
            "<h3>LoC budget</h3><table class=\"kv\"><tr><td>Pacer</td><td>{}{}</td></tr>\
             <tr><td>Sent</td><td>{} requests since the pacer started</td></tr>\
             <tr><td>State</td><td>written {}</td></tr></table>\
             <p class=\"note\">Every LoC request from the scan fetcher and the bulk download goes through this pacer.</p>",
            if p.slowed.as_ref().is_some_and(|v| !v.is_null()) {
                badge("warning", "slowed: LoC pushed back")
            } else {
                badge("good", "normal")
            },
            until.map_or_else(String::new, |t| format!(" {}", badge("warning", &format!("hold until {}", time(t))))),
            int(p.sent),
            f.pacer.modified.map_or_else(|| "-".to_owned(), |t| ago(now, t))
        );
    } else if let Some(e) = &f.pacer.error {
        let _ = write!(o, "<p class=\"err\">LoC pacer: {}</p>", esc(e));
    }
    if f.publish.configured {
        o.push_str("<h3>Publish fetched</h3>");
        if let Some(p) = &f.publish.value {
            let state = p.state(now);
            let _ = write!(
                o,
                "<p>{} Last completion: {}. Systemd result: {}; exit status: {}.</p>",
                badge(
                    match state {
                        "succeeded" => "good",
                        "running" => "warning",
                        _ => "critical",
                    },
                    state,
                ),
                p.finished_at
                    .map_or_else(|| "not observed".to_owned(), time),
                esc(p.result.as_deref().unwrap_or("not observed")),
                p.exit_status
                    .map_or_else(|| "not observed".to_owned(), |s| s.to_string()),
            );
            if let Some(error) = &p.error {
                let _ = write!(o, "<p class=\"err\">{}</p>", esc(error));
            }
        } else if let Some(error) = &f.publish.error {
            let _ = write!(
                o,
                "<p class=\"err\">Publisher metadata unknown: {}</p>",
                esc(error)
            );
        }
        o.push_str("<p class=\"note\">Completion is observed from systemd, not inferred from fetched markers. A successful run does not prove that new pages were published. Metadata may be unavailable after a restart.</p>");
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
        let _ = write!(
            o,
            "<h3>By set, plan <code>{}</code>{}</h3><table><tr><th>Set</th><th>Fetched</th><th class=\"r\">Data</th></tr>",
            esc(&f.plan.as_deref().map(manifest_name).unwrap_or_default()),
            if f.plan_pinned { " (pinned)" } else { "" }
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
        "<h3>Failures{}</h3><p>{} pages LoC can't serve (permanent failures). {} pages of queued manifests skipped for now because \
         loc.gov answers 503 for their issue while serving others (<code>loc_item_error</code>); the queue runs their manifest again after a day's cool-off. \
         {} other pages skipped this way are in no queued manifest, so they are asked for again only if they are queued again.</p>",
        if f.recent_failures.is_empty() {
            String::new()
        } else {
            format!(" ({} latest)", f.recent_failures.len())
        },
        int(f.dead_pages as u64),
        int(f.cooling_pages as u64),
        int(f.cooling_unqueued as u64)
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
}

pub(super) fn fetch_queue(v: &View, o: &mut String) {
    let now = v.generated_at;
    let f = &v.fetch;
    o.push_str("<section><h2>Fetch queue</h2>");
    if let Some(e) = &f.queue_error {
        let _ = write!(o, "<p class=\"err\">{}</p>", esc(e));
    }
    if !f.queue_configured {
        o.push_str("<p class=\"muted\">No manifests directory configured.</p>");
    } else if f.queue.is_empty() {
        o.push_str("<p class=\"muted\">No queued manifests.</p>");
    } else {
        o.push_str(&scroll("Fetch queue"));
        o.push_str("<table><tr><th>Manifest</th><th>Kind</th><th>State</th><th>Fetched</th></tr>");
        for m in &f.queue {
            let state = match m.state {
                "fetched" => badge("good", "fetched"),
                "fetching" => badge("good", "fetching"),
                "cooling" => badge("warning", "cooling off"),
                other => badge("warning", other),
            };
            let _ = write!(
                o,
                "<tr><td><code>{}</code></td><td>{}</td><td>{state}{}</td><td>{}</td></tr>",
                esc(&m.name),
                m.kind,
                m.marked_at
                    .map_or_else(String::new, |t| format!(" {}", ago(now, t))),
                match &m.error {
                    Some(e) => format!("<span class=\"err\">{}</span>", esc(e)),
                    None => meter(
                        m.fetched,
                        m.pages,
                        &format!("{} of {} pages", int(m.fetched), int(m.pages))
                    ),
                }
            );
        }
        o.push_str("</table></div>");
    }
    o.push_str(
        "<p class=\"note\">The queue runs OCR's manifests first, then the quality pass's, each in name order. \
         Fetched counts every page of a manifest in the fetch index, by any run.</p>",
    );
    o.push_str("<h3>Bulk OCR archives</h3>");
    match (&f.bulk.value, &f.bulk.error) {
        (Some(b), _) => {
            let _ = write!(
                o,
                "<table class=\"kv\"><tr><td>Download</td><td>{} {}</td></tr>\
                 <tr><td>Archives</td><td>{}</td></tr>\
                 <tr><td>Data</td><td>{}</td></tr>\
                 <tr><td>This run</td><td>{} archives, {}; {}/s</td></tr>\
                 <tr><td>Failed</td><td>{}</td></tr>\
                 <tr><td>Status</td><td>written {}; done in {}</td></tr></table>",
                state_badge(&b.state),
                if f.bulk
                    .modified
                    .is_some_and(|t| now - t > chrono::Duration::minutes(30))
                {
                    badge("warning", "stale")
                } else {
                    String::new()
                },
                meter(
                    b.done,
                    b.archives,
                    &format!("{} of {}", int(b.done), int(b.archives))
                ),
                meter(
                    b.bytes_done,
                    b.bytes_total,
                    &format!("{} of {}", bytes(b.bytes_done), bytes(b.bytes_total))
                ),
                int(b.fetched_this_run),
                bytes(b.bytes_this_run),
                bytes(b.bytes_per_sec.max(0.0) as u64),
                int(b.failed.len() as u64),
                time_str(&b.updated_at),
                bulk_eta(b).map_or_else(|| "-".to_owned(), span)
            );
        }
        (None, Some(e)) => {
            let _ = write!(o, "<p class=\"err\">{}</p>", esc(e));
        }
        (None, None) => o.push_str(
            "<p class=\"muted\">The bulk download's status file isn't configured here.</p>",
        ),
    }
    o.push_str("</section>");
}

pub(super) fn serial_set(v: &View, o: &mut String) {
    let now = v.generated_at;
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
}

pub(super) fn nodes(v: &View, o: &mut String) {
    let now = v.generated_at;
    o.push_str("<section><h2>Nodes</h2>");
    if v.nodes.is_empty() {
        o.push_str("<p class=\"muted\">No nodes configured.</p>");
    }
    for n in &v.nodes {
        let state = match n.state {
            "working" | "idle" => badge("good", "reporting"),
            "retired" => badge("warning", "retired"),
            _ => badge("critical", "no status"),
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
        if !crate::view::node_status_fresh(n, now) {
            let _ = write!(
                o,
                "<p class=\"muted\">{}</p>",
                if n.state == "retired" {
                    "Retired: no answer for over a day. OCR runs through BOINC (#1053); its counters aren't shown."
                } else {
                    "No current status; its counters aren't shown."
                }
            );
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
}

pub(super) fn whats_next(v: &View, o: &mut String) {
    let x = &v.next;
    o.push_str("<section><h2>What's next</h2>");
    let _ = write!(
        o,
        "<h3>To fetch (next {}): queued manifests, in the queue's order</h3>",
        x.to_fetch.len()
    );
    next_table(o, "To fetch", &x.to_fetch);
    o.push_str(
        "<p class=\"note\">What is OCRed next is decided by the on-prem OCR queue and BOINC, \
         which this dashboard doesn't read yet.</p>",
    );
    o.push_str("<h3>Seed plan</h3>");
    match &x.seed_plan {
        Some(p) => {
            o.push_str("<table class=\"kv\">");
            for (k, val) in p {
                let _ = write!(o, "<tr><td>{}</td><td>{}</td></tr>", esc(k), esc(val));
            }
            o.push_str("</table>");
        }
        None => o.push_str("<p class=\"note\">No seed plan file. The seeder's settings (USNM_OCR_SEED_*) are on the on-prem ocr-sync timer in the build container.</p>"),
    }
    o.push_str("</section>");
}

pub(super) fn ocred(v: &View, o: &mut String) {
    let d = &v.ocred;
    o.push_str("<section><h2>What's been OCRed</h2>");
    let _ = write!(
        o,
        "<p>{} pages OCRed by the direct queue workers, from the journals copied here ({}){}. \
         Those workers are retired (#1053): OCR now runs through BOINC, whose output isn't counted here. \
         The pages whose searchable text is our OCR are in the corpus data flow's search-inclusion stage.</p>",
        int(d.pages),
        if d.journals.is_empty() { "none".to_owned() } else { esc(&d.journals.join(", ")) },
        d.last_at.map_or_else(String::new, |t| format!(", newest page {}", time(t)))
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
    pairs(o, "By language", &d.by_language);
    pairs(o, "By engine", &d.by_engine);
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
}

pub(super) fn queues(v: &View, o: &mut String) {
    let now = v.generated_at;
    o.push_str("<section><h2>Queues and dead letters</h2>");
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
            if v.azure.queues_metrics_only {
                o.push_str("<p class=\"muted\">Dead-letter message bodies are not read by this metrics-only source.</p>");
            } else if q.dead_letters.is_empty() {
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
        _ => {
            if let Some(error) = &v.azure.queues_error {
                let _ = write!(
                    o,
                    "<p class=\"err\">Queue metrics unavailable: {}</p>",
                    esc(error)
                );
            } else if v.azure.queues_metrics_only {
                o.push_str("<p class=\"muted\">No successful pgmq observation yet.</p>");
            } else {
                o.push_str(
                    "<p class=\"muted\">No queue counts: no configured worker reports them.</p>",
                );
            }
        }
    }
    let _ = write!(
        o,
        "<p class=\"note\">{}</p></section>",
        esc(v.azure.queues_note)
    );
}
