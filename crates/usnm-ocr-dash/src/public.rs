//! The public, read-only view (12 §12.2.5): what the internet sees through
//! the tunnel. The same view as the LAN's, with what names the lab taken
//! out: the Azure workbook link (it carries subscription and tenant ids),
//! host, node and guest names, container and VM numbers, file system
//! paths, IP addresses and the seed plan's settings.

use std::collections::BTreeMap;

use crate::view::View;

/// Where a path starts that the page must not show.
const PATH_ROOTS: &[&str] = &[
    "/tank", "/var/", "/srv/", "/etc/", "/home/", "/opt/", "/root", "/run/", "/tmp/", "/usr/",
    "/mnt/", "/scratch",
];

fn word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | '~')
}

/// One word of free text, or `None` to keep it.
fn label_for(word: &str) -> Option<&'static str> {
    let lower = word.trim_end_matches(['.', '-']).to_ascii_lowercase();
    if PATH_ROOTS.iter().any(|r| lower.starts_with(r)) {
        return Some("a local path");
    }
    // Any other path, absolute or relative (`/data/scans/x.json`,
    // `tank/ocr/scans/…`), but not a URL's remainder (`//host/…`).
    if !lower.starts_with("//") && lower.contains('/') {
        let last = lower.rsplit('/').next().unwrap_or_default();
        let absolute = lower.starts_with('/') && lower.len() > 1;
        if absolute || lower.starts_with("tank/") || last.contains('.') {
            return Some("a local path");
        }
    }
    // A host, alone or at the start of a URL's remainder (`//host/…`).
    let host = lower
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or_default();
    if host.contains("tjg.fyi") || host == "pve" || host.starts_with("pve-") {
        return Some("a lab host");
    }
    if host.contains("ocr-node") {
        return Some("an OCR node");
    }
    let quad = host.split('.').count() == 4 && host.split('.').all(|p| p.parse::<u8>().is_ok());
    if quad || host.parse::<std::net::Ipv6Addr>().is_ok() {
        return Some("an address");
    }
    None
}

/// Free text (an error, a note) with the lab's names, paths, container
/// numbers and addresses replaced by generic words.
pub fn scrub(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        // An IPv6 literal, alone or in brackets, before sentence punctuation.
        let at_start = i == 0 || !chars[i - 1].is_ascii_alphanumeric();
        if at_start && (chars[i].is_ascii_hexdigit() || chars[i] == ':') {
            let mut j = i;
            while j < chars.len() && (chars[j].is_ascii_hexdigit() || matches!(chars[j], ':' | '.'))
            {
                j += 1;
            }
            let run: String = chars[i..j].iter().collect();
            let lit = run.trim_end_matches('.');
            let next_word = chars.get(j).is_some_and(|c| c.is_ascii_alphanumeric());
            if !next_word
                && lit.matches(':').count() >= 2
                && lit.parse::<std::net::Ipv6Addr>().is_ok()
            {
                out.push_str("an address");
                out.push_str(&run[lit.len()..]);
                i = j;
                continue;
            }
        }
        if !word_char(chars[i]) {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && word_char(chars[i]) {
            i += 1;
        }
        let word: String = chars[start..i].iter().collect();
        // "CT 212", "VM 104", "CT212", "VM205".
        let numbered = |w: &str| {
            let (label, rest) = if let Some(r) = w.strip_prefix("CT") {
                ("a container", r)
            } else {
                ("a VM", w.strip_prefix("VM")?)
            };
            (!rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())).then_some(label)
        };
        if (word == "CT" || word == "VM")
            && chars.get(i) == Some(&' ')
            && chars.get(i + 1).is_some_and(char::is_ascii_digit)
        {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            out.push_str(if word == "CT" { "a container" } else { "a VM" });
            i = j;
            continue;
        }
        let core = word.trim_end_matches(['.', '-']);
        if let Some(label) = numbered(core) {
            out.push_str(label);
            out.push_str(&word[core.len()..]);
            continue;
        }
        // An IPv6 address or `host:port` spans colons: look at the whole run.
        let mut j = i;
        while j < chars.len() && (word_char(chars[j]) || chars[j] == ':') {
            j += 1;
        }
        let wide: String = chars[start..j].iter().collect();
        if wide.contains(':') && !wide.ends_with(':') {
            if let Some((host, _port)) = wide.rsplit_once(':') {
                if label_for(host).is_some() || wide.parse::<std::net::Ipv6Addr>().is_ok() {
                    out.push_str(label_for(host).unwrap_or("an address"));
                    i = j;
                    continue;
                }
            }
        }
        match label_for(core) {
            Some(l) => {
                out.push_str(l);
                out.push_str(&word[core.len()..]);
            }
            None => out.push_str(&word),
        }
    }
    out
}

fn scrub_opt(s: &mut Option<String>) {
    if let Some(x) = s {
        *x = scrub(x);
    }
}

/// A manifest's version, without its path (`queue-ocr-…`).
pub fn manifest_name(path: &str) -> String {
    let p = std::path::Path::new(path);
    p.parent()
        .and_then(|d| d.file_name())
        .or_else(|| p.file_name())
        .map_or_else(|| path.to_owned(), |n| n.to_string_lossy().into_owned())
}

/// Take out of the view everything the public page doesn't show.
pub fn redact(v: &mut View) {
    v.public = true;

    // Hosts: the one with the scans' datasets is the scan server.
    let mut lab = 0;
    for h in &mut v.hosts {
        h.name = if h.datasets.is_empty() {
            lab += 1;
            format!("lab host {lab}")
        } else {
            "scan server".to_owned()
        };
        scrub_opt(&mut h.error);
        for (i, p) in h.pools.iter_mut().enumerate() {
            let label = format!("pool {}", i + 1);
            let old = std::mem::replace(&mut p.name, label.clone());
            for s in [&mut p.status_x, &mut p.scan, &mut p.errors] {
                *s = scrub(&s.replace(&format!("'{old}'"), &format!("'{label}'")));
            }
        }
        for d in &mut h.datasets {
            // `tank/ocr/scans` → `scans`.
            d.name = d.name.rsplit('/').next().unwrap_or_default().to_owned();
        }
        let (mut cts, mut vms) = (0, 0);
        for g in &mut h.guests {
            g.vmid = 0;
            g.name = if g.kind == "lxc" {
                cts += 1;
                format!("container {cts}")
            } else {
                vms += 1;
                format!("VM {vms}")
            };
        }
    }

    // Throughput: counts only, but limits and failure reasons are free text.
    for s in &mut v.throughput.steps {
        scrub_opt(&mut s.limit);
        scrub_opt(&mut s.detail.limit_now);
        if let Some(f) = &mut s.detail.failures {
            for r in &mut f.reasons {
                r.text = scrub(&r.text);
            }
        }
    }
    scrub_opt(&mut v.throughput.error);

    // Nodes: numbered, without their addresses.
    let mut names: BTreeMap<String, String> = BTreeMap::new();
    for (i, n) in v.nodes.iter_mut().enumerate() {
        let label = format!("OCR node {}", i + 1);
        names.insert(n.name.clone(), label.clone());
        n.name = label;
        n.url = String::new();
        n.node_id = String::new();
        n.scan_cache_url = None;
        n.routes.clear();
        scrub_opt(&mut n.error);
        for f in &mut n.in_flight {
            f.queue = scrub(&f.queue);
        }
    }
    let label_of = |names: &mut BTreeMap<String, String>, name: &str| -> String {
        let next = names.len() + 1;
        names
            .entry(name.to_owned())
            .or_insert_with(|| format!("OCR node {next}"))
            .clone()
    };
    v.ocred.journals = v
        .ocred
        .journals
        .iter()
        .map(|j| label_of(&mut names, j))
        .collect();
    for l in &mut v.ocred.latest {
        l.node = label_of(&mut names, &l.node);
        for c in &mut l.rows {
            c.scan = scrub(&c.scan);
            c.reason = scrub(&c.reason);
            c.queue = scrub(&c.queue);
        }
    }
    v.ocred.workbook_url = None;
    v.azure.workbook_url = None;
    if !v.azure.queues_metrics_only {
        v.azure.queues_from = v
            .azure
            .queues_from
            .as_deref()
            .map(|n| label_of(&mut names, n));
    }
    scrub_opt(&mut v.azure.queues_error);
    if let Some(q) = &mut v.azure.queues {
        scrub_opt(&mut q.error);
        for d in &mut q.dead_letters {
            d.node = label_of(&mut names, &d.node);
            d.from = scrub(&d.from);
            d.cause = scrub(&d.cause);
            d.error = scrub(&d.error);
        }
    }

    // The fetch: manifests by name, no paths.
    let f = &mut v.fetch;
    if let Some(s) = &mut f.status {
        s.manifest = manifest_name(&s.manifest);
        s.out = String::new();
        s.pid = 0;
        scrub_opt(&mut s.error);
    }
    f.plan = f.plan.as_deref().map(manifest_name);
    for e in [
        &mut f.status_error,
        &mut f.plan_error,
        &mut f.index_error,
        &mut f.queue_error,
        &mut f.bulk.error,
        &mut f.pacer.error,
        &mut f.publish.error,
    ] {
        scrub_opt(e);
    }
    if let Some(publish) = &mut f.publish.value {
        scrub_opt(&mut publish.error);
        scrub_opt(&mut publish.active_state);
        scrub_opt(&mut publish.result);
    }
    for m in &mut f.queue {
        m.path = String::new();
        scrub_opt(&mut m.error);
    }
    if let Some(b) = &mut f.bulk.value {
        b.failed = b
            .failed
            .iter()
            .map(|x| serde_json::Value::String(scrub(&x.to_string())))
            .collect();
    }
    for l in &mut f.recent_failures {
        l.error = scrub(&l.error);
    }

    for x in &mut v.serial_set {
        x.name = "Serial Set fetch".to_owned();
        x.status.unit = String::new();
        scrub_opt(&mut x.status.current_volume);
    }
    for r in &mut v.next.to_fetch {
        r.manifest = manifest_name(&r.manifest);
    }
    // Its keys and values are free-form settings.
    v.next.seed_plan = None;

    let p = &mut v.pipeline;
    scrub_opt(&mut p.last_error);
    for f in &mut p.stage_freshness {
        f.id = scrub_id(&f.id, &mut names);
    }
    if let Some(s) = &mut p.snapshot {
        s.producer = scrub(&s.producer);
        for c in &mut s.coverage {
            *c = scrub(c);
        }
        for st in &mut s.stages {
            st.id = scrub_id(&st.id, &mut names);
            st.label = scrub(&st.label);
            st.detail = scrub(&st.detail);
        }
        for i in &mut s.issues {
            i.code = scrub_id(&i.code, &mut names);
            i.detail = scrub(&i.detail);
        }
    }
}

/// An identifier (`local_usnm-ocr-node3_completed_hour`) with each worker
/// name in it replaced by the same number the page gives that worker
/// (`local_worker1_completed_hour`), so ids stay distinct, then scrubbed.
fn scrub_id(id: &str, names: &mut BTreeMap<String, String>) -> String {
    const NODE: &str = "usnm-ocr-node";
    let mut out = String::new();
    let mut rest = id;
    while let Some(at) = rest.find(NODE) {
        out.push_str(&rest[..at]);
        let after = &rest[at + NODE.len()..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        let name = &rest[at..at + NODE.len() + digits];
        let next = names.len() + 1;
        let label = names
            .entry(name.to_owned())
            .or_insert_with(|| format!("OCR node {next}"));
        out.push_str("worker");
        out.push_str(label.trim_start_matches("OCR node "));
        rest = &after[digits..];
    }
    out.push_str(rest);
    // `_` joins words in an id: scrub each part.
    out.split('_').map(scrub).collect::<Vec<_>>().join("_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_replaces_lab_words_and_keeps_the_rest() {
        let s = scrub(
            "can't read /tank/ocr/scans/status-full.json on pve-big (CT 212), \
             usnm-ocr-node3 at 192.0.2.7:8381 via ocr.tjg.fyi, http://192.0.2.8:8381/status; [2001:db8::1]:80; VM205 down",
        );
        for bad in [
            "/tank", "pve", "CT 212", "node3", "192.0.2", "tjg", "2001:db8", "VM205",
        ] {
            assert!(!s.contains(bad), "{bad} in {s}");
        }
        assert!(s.starts_with("can't read a local path on a lab host (a container)"));
        assert!(s.ends_with("a VM down"), "{s}");
        for leak in [
            "could not start VM205.",
            "CT212.",
            "at [::1]:8381 and [2001:db8::]:8381, or 2001:db8::7.",
            "missing /data/scans/status-full.json, tank/ocr/scans/status-full.json",
        ] {
            let s = scrub(leak);
            for bad in ["205", "212", "::", "db8", "/data", "tank/", "status-full"] {
                assert!(!s.contains(bad), "{bad} in {s}");
            }
        }
        assert_eq!(scrub("could not start VM205."), "could not start a VM.");
        assert_eq!(
            scrub("at 19:24:44 runs:observed"),
            "at 19:24:44 runs:observed"
        );
        let mut names = BTreeMap::new();
        assert_eq!(
            scrub_id("local_usnm-ocr-node3_completed_hour", &mut names),
            "local_worker1_completed_hour"
        );
        assert_eq!(scrub_id("usnm-ocr-node4", &mut names), "worker2");
        let keep = "https://tile.loc.gov/storage-services/x/0746.jp2: holding, v1.2.3";
        assert_eq!(scrub(keep), keep);
    }

    #[test]
    fn manifest_names_lose_their_path() {
        assert_eq!(
            manifest_name("/tank/ocr/manifests/queue-ocr-1/manifest.parquet"),
            "queue-ocr-1"
        );
    }
}
