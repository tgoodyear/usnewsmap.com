//! Escaping and display formatting shared by HTML panels.

use chrono::{DateTime, Utc};

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

pub(super) fn pct(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        (part as f64 / whole as f64 * 100.0).min(100.0)
    }
}

/// A time, in US Eastern once the page's script runs, UTC before.
pub(super) fn time(t: DateTime<Utc>) -> String {
    let iso = t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    format!(
        "<time datetime=\"{iso}\">{}</time>",
        t.format("%Y-%m-%d %H:%M:%S UTC")
    )
}

/// [`time`] for an RFC 3339 string; the string itself if it isn't one.
pub(super) fn time_str(s: &str) -> String {
    match DateTime::parse_from_rfc3339(s) {
        Ok(t) => time(t.with_timezone(&Utc)),
        Err(_) => esc(s),
    }
}

pub(super) fn ago(now: DateTime<Utc>, t: DateTime<Utc>) -> String {
    let s = (now - t).num_seconds().max(0) as u64;
    format!("{} ago", span(s))
}

/// A short form that opens, by click, tap or keyboard, to the whole of it.
pub(super) fn more(short: &str, whole: &str) -> String {
    format!("<details class=\"more\"><summary>{short}</summary><div>{whole}</div></details>")
}

/// Text cut to one line with an ellipsis, opening to the whole of it.
pub(super) fn clip(s: &str) -> String {
    more(&format!("<span class=\"clip\">{}</span>", esc(s)), &esc(s))
}

/// A progress track with its label beside it.
pub(super) fn meter(part: u64, whole: u64, label: &str) -> String {
    let p = pct(part, whole);
    format!(
        "<span class=\"meter\" role=\"meter\" aria-valuemin=\"0\" aria-valuemax=\"100\" aria-valuenow=\"{p:.1}\"><span style=\"width:{p:.2}%\"></span></span> <span class=\"num\">{}</span>",
        esc(label)
    )
}

/// A state with its icon and word, never color alone.
pub(super) fn badge(kind: &str, text: &str) -> String {
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

/// A list that scrolls inside its panel once it is long.
pub(super) fn scroll(label: &str) -> String {
    format!(
        "<div class=\"scroll\" tabindex=\"0\" role=\"region\" aria-label=\"{}\">",
        esc(label)
    )
}

/// `https://www.loc.gov/item/...` → `/item/...`: the host adds nothing in a
/// list where every address is LoC's.
pub(super) fn short_urls(s: &str) -> String {
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
pub(super) fn msg(e: &str, at: Option<DateTime<Utc>>) -> String {
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

/// A manifest's version: the directory it is in (`queue-ocr-…`).
pub(super) fn manifest_name(path: &str) -> String {
    let p = std::path::Path::new(path);
    p.parent()
        .and_then(|d| d.file_name())
        .or_else(|| p.file_name())
        .map_or_else(|| path.to_owned(), |n| n.to_string_lossy().into_owned())
}

pub(super) fn state_badge(state: &str) -> String {
    match state {
        "running" | "done" | "finished" => badge("good", state),
        "waiting" | "budget" | "stopped" | "cooling" => badge("warning", state),
        other => badge("critical", other),
    }
}

/// Until the bulk download is done, at its rate.
pub(super) fn bulk_eta(b: &crate::fetch::BulkStatus) -> Option<u64> {
    (b.state == "running" && b.bytes_per_sec > 0.0)
        .then(|| (b.bytes_total.saturating_sub(b.bytes_done) as f64 / b.bytes_per_sec) as u64)
}

/// The fetcher's activity, as a badge.
pub(super) fn fetch_badge(f: &crate::view::FetchView) -> String {
    match f.activity {
        "running" => state_badge(f.status.as_ref().map_or("running", |s| s.state.as_str())),
        "idle" => badge("good", "idle, queue empty"),
        "queued" => badge(
            "warning",
            &format!(
                "{} queued, no current status",
                int((f.queue_summary.queued + f.queue_summary.fetching) as u64)
            ),
        ),
        _ => badge("warning", "unknown"),
    }
}

pub(super) fn sset_badge(x: &crate::view::SsetView) -> String {
    let b = match x.state.as_str() {
        "finished" => badge("good", "finished"),
        "running" => badge("good", "running"),
        "unknown" => badge("warning", "unknown or stopped"),
        other => badge("warning", other),
    };
    if x.stale {
        format!("{b} {}", badge("warning", "stale"))
    } else {
        b
    }
}

pub(super) fn sset_eta(x: &crate::view::SsetView) -> String {
    if x.state == "finished" {
        return "done".to_owned();
    }
    if x.stale {
        return "-".to_owned();
    }
    x.status
        .eta_hours
        .map_or_else(|| "-".to_owned(), |h| span((h * 3600.0) as u64))
}
