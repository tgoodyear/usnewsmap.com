//! GHN's IIIF Presentation 2.0 documents, as its Open ONI site serves them: the newspapers list,
//! a title's collection of issues, and an issue's manifest of pages. Only what's needed is
//! read, and every URL in them is checked against the shape GHN uses, so a document can't send
//! the fetcher anywhere else.

use anyhow::{bail, ensure, Context};
use chrono::NaiveDate;
use serde_json::Value;
use usnm_core::ids::PageKey;
use usnm_core::page_id::{PageId, Source};

/// Where GHN's documents and page images are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hosts {
    /// The site, with no trailing `/`.
    pub site: String,
    /// The image service's prefix for newspaper pages, ending in `/`.
    pub images: String,
}

impl Hosts {
    pub fn ghn() -> Self {
        Self {
            site: super::SITE.to_owned(),
            images: super::IMAGES.to_owned(),
        }
    }

    pub fn newspapers(&self) -> String {
        format!("{}/newspapers.json", self.site)
    }

    pub fn title(&self, lccn: &str) -> String {
        format!("{}/lccn/{lccn}.json", self.site)
    }

    /// The issue a manifest URL names, if it is one.
    pub fn issue_of(&self, url: &str) -> Option<IssueRef> {
        let rest = url
            .strip_prefix(&self.site)?
            .strip_prefix("/lccn/")?
            .strip_suffix(".json")?;
        let [lccn, date, ed] = rest.split('/').collect::<Vec<_>>()[..] else {
            return None;
        };
        let date = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
        let edition = ed.strip_prefix("ed-")?.parse().ok().filter(|n| *n > 0)?;
        let issue = IssueRef {
            lccn: lccn.to_owned(),
            date,
            edition,
        };
        // Only the canonical spelling: `ed-01` or `1876-1-5` would be a second name.
        (lccn_ok(lccn) && issue.url(self) == url).then_some(issue)
    }
}

/// An LCCN as page keys allow it.
pub fn lccn_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 16
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// A title in GHN's newspapers list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Title {
    pub lccn: String,
    pub label: String,
}

/// One issue of a title: its manifest is [`IssueRef::url`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IssueRef {
    pub lccn: String,
    pub date: NaiveDate,
    pub edition: u16,
}

impl IssueRef {
    pub fn url(&self, hosts: &Hosts) -> String {
        format!(
            "{}/lccn/{}/{}/ed-{}.json",
            hosts.site,
            self.lccn,
            self.date.format("%Y-%m-%d"),
            self.edition
        )
    }

    /// `<lccn>_<date>_ed-<n>`, for file names.
    pub fn stem(&self) -> String {
        format!(
            "{}_{}_ed-{}",
            self.lccn,
            self.date.format("%Y-%m-%d"),
            self.edition
        )
    }
}

/// A title's issues, from its collection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleIssues {
    pub label: String,
    pub issues: Vec<IssueRef>,
}

/// A page of an issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub key: PageKey,
    /// `ghn:<lccn>_<date>_ed-<n>_seq-<n>` (ADR-0026).
    pub id: PageId,
    /// The page's IIIF Image API service.
    pub service: String,
    /// The full-size JPEG: `<service>/full/full/0/default.jpg`.
    pub image: String,
    pub width: Option<u64>,
    pub height: Option<u64>,
}

fn id_of(v: &Value) -> Option<&str> {
    v.get("@id").and_then(Value::as_str)
}

fn check_doc(v: &Value, kind: &str, url: &str) -> anyhow::Result<()> {
    ensure!(
        v.get("@type").and_then(Value::as_str) == Some(kind),
        "{url}: not an {kind}"
    );
    ensure!(id_of(v) == Some(url), "{url}: its @id is {:?}", id_of(v));
    Ok(())
}

/// The titles in `/newspapers.json`.
pub fn newspapers(v: &Value, hosts: &Hosts) -> anyhow::Result<Vec<Title>> {
    let url = hosts.newspapers();
    check_doc(v, "sc:Collection", &url)?;
    let list = v
        .get("collections")
        .and_then(Value::as_array)
        .with_context(|| format!("{url}: no collections"))?;
    let prefix = format!("{}/lccn/", hosts.site);
    let mut titles = Vec::with_capacity(list.len());
    for c in list {
        let id = id_of(c).with_context(|| format!("{url}: a title with no @id"))?;
        let lccn = id
            .strip_prefix(&prefix)
            .and_then(|r| r.strip_suffix(".json"))
            .filter(|l| lccn_ok(l))
            .with_context(|| format!("{url}: not a title: {id}"))?;
        let label = c.get("label").and_then(Value::as_str).unwrap_or_default();
        titles.push(Title {
            lccn: lccn.to_owned(),
            label: label.to_owned(),
        });
    }
    titles.sort_by(|a, b| a.lccn.cmp(&b.lccn));
    if let Some(w) = titles.windows(2).find(|w| w[0].lccn == w[1].lccn) {
        bail!("{url}: {} is listed twice", w[0].lccn);
    }
    Ok(titles)
}

/// The issues in a title's collection, `/lccn/<lccn>.json`.
pub fn title(v: &Value, lccn: &str, hosts: &Hosts) -> anyhow::Result<TitleIssues> {
    let url = hosts.title(lccn);
    check_doc(v, "sc:Collection", &url)?;
    let list = v
        .get("manifests")
        .and_then(Value::as_array)
        .with_context(|| format!("{url}: no manifests"))?;
    let mut issues = Vec::with_capacity(list.len());
    for m in list {
        let id = id_of(m).unwrap_or_default();
        let issue = hosts
            .issue_of(id)
            .filter(|i| i.lccn == lccn)
            .with_context(|| format!("{url}: not one of its issues: {id}"))?;
        issues.push(issue);
    }
    issues.sort();
    issues.dedup();
    let label = v.get("label").and_then(Value::as_str).unwrap_or_default();
    Ok(TitleIssues {
        label: label.to_owned(),
        issues,
    })
}

/// The pages in an issue's manifest, in sequence order.
pub fn issue(v: &Value, want: &IssueRef, hosts: &Hosts) -> anyhow::Result<Vec<Page>> {
    let url = want.url(hosts);
    check_doc(v, "sc:Manifest", &url)?;
    let sequences = v
        .get("sequences")
        .and_then(Value::as_array)
        .with_context(|| format!("{url}: no sequences"))?;
    let [sequence] = sequences.as_slice() else {
        bail!("{url}: {} sequences, not one", sequences.len());
    };
    let canvases = sequence
        .get("canvases")
        .and_then(Value::as_array)
        .filter(|c| !c.is_empty())
        .with_context(|| format!("{url}: no pages"))?;
    let page_prefix = format!("{}/seq-", url.trim_end_matches(".json"));
    let mut pages = Vec::with_capacity(canvases.len());
    for (i, c) in canvases.iter().enumerate() {
        let n = i + 1;
        let image = c
            .get("images")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .with_context(|| format!("{url}: canvas {n} has no image"))?;
        let service = image
            .pointer("/resource/service/@id")
            .and_then(Value::as_str)
            .with_context(|| format!("{url}: canvas {n} has no image service"))?;
        let ident = service
            .strip_prefix(&hosts.images)
            .with_context(|| format!("{url}: canvas {n}'s image service isn't GHN's: {service}"))?;
        // One path segment: GHN writes the identifier's own slashes as %2F.
        let ident_ok = !ident.is_empty()
            && ident
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-%".contains(&b));
        ensure!(
            ident_ok,
            "{url}: canvas {n}'s image identifier isn't one path segment of letters, digits, \
             '.', '_', '-' and '%': {service}"
        );
        // The page's number is GHN's own, from its page link, never the canvas's position.
        let seq = image
            .get("rendering")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|r| r.get("format").and_then(Value::as_str) == Some("text/html"))
            .filter_map(|r| id_of(r)?.strip_prefix(&page_prefix)?.strip_suffix('/'))
            .find(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|s| s.parse::<u16>().ok())
            .with_context(|| format!("{url}: canvas {n} has no page link"))?;
        let key = PageKey::new(&want.lccn, want.date, want.edition, seq)
            .with_context(|| format!("{url}: canvas {n}"))?;
        let id = PageId::new(Source::Ghn, &key.doc_id())?;
        pages.push(Page {
            key,
            id,
            service: service.to_owned(),
            image: format!("{service}/full/full/0/default.jpg"),
            width: c.get("width").and_then(Value::as_u64),
            height: c.get("height").and_then(Value::as_u64),
        });
    }
    pages.sort_by_key(|p| p.key.seq);
    if let Some(w) = pages.windows(2).find(|w| w[0].key.seq == w[1].key.seq) {
        bail!("{url}: two pages are seq-{}", w[0].key.seq);
    }
    Ok(pages)
}
