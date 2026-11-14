//! `--action plan`: walk the collection and its manifests, save every document verbatim, and
//! write `plan.json`: the issues, canvases and image URLs, with counts and an estimated
//! duration at the pace. No image is fetched.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::watch;

use super::iiif::{self, Citation};
use super::{json_of, template, Action, Client, InfoJson, Options, Summary};

/// Collection documents read at most (a loop in `next` links stops at the visited set).
const MAX_COLLECTION_DOCS: usize = 10_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlanCanvas {
    pub index: usize,
    pub canvas_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub url: String,
    /// The image service's base URL, when the image came from a service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    /// The Image API `size` segment asked for (`full`, `w,`, ...), or `as-given`.
    pub size: String,
    pub ext: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlanIssue {
    pub id: String,
    pub manifest_url: String,
    pub manifest_sha256: String,
    pub citation: Citation,
    pub canvases: Vec<PlanCanvas>,
    /// Canvases with no image, and why.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Plan {
    pub version: u32,
    pub created_at: String,
    pub collection_url: String,
    /// The collection's licence/terms URL, which the tool never fetches.
    pub terms_url: Option<String>,
    pub attribution: Option<Value>,
    pub collection_total: Option<u64>,
    pub user_agent: String,
    pub gap_secs: f64,
    pub issues_count: usize,
    pub images_count: usize,
    pub estimated_requests: usize,
    pub estimated_seconds: u64,
    pub issues: Vec<PlanIssue>,
}

/// What the operator must do about the terms; printed before fetching.
pub fn terms_notice(terms: Option<&str>) -> String {
    match terms {
        Some(t) => format!(
            "The collection names these terms of use: {t}\nYou must have read them before \
             fetching. This tool does not fetch that page."
        ),
        None => {
            "The collection names no terms of use. Find and read them before fetching.".to_owned()
        }
    }
}

/// A directory name for a manifest, from its URL's last meaningful segment.
fn slug(url: &str) -> String {
    let path = reqwest::Url::parse(url)
        .map(|u| u.path().to_owned())
        .unwrap_or_default();
    let mut segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    while segs.len() > 1 && matches!(segs.last(), Some(&("manifest" | "manifest.json"))) {
        segs.pop();
    }
    let last = segs.last().copied().unwrap_or("issue");
    let last = last.strip_suffix(".json").unwrap_or(last);
    let mut s: String = last
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    if !s.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        s.insert(0, 'i');
    }
    s
}

/// Unique ids for `urls`: the slug, with a short hash of the URL on those that collide.
pub fn issue_ids(urls: &[String]) -> Vec<String> {
    let slugs: Vec<String> = urls.iter().map(|u| slug(u)).collect();
    let mut count: HashMap<&str, usize> = HashMap::new();
    for s in &slugs {
        *count.entry(s).or_default() += 1;
    }
    urls.iter()
        .zip(&slugs)
        .map(|(u, s)| {
            if count[s.as_str()] > 1 {
                let h = crate::ghn::sha256_hex(u.as_bytes());
                format!("{s}-{}", &h[..8])
            } else {
                s.clone()
            }
        })
        .collect()
}

pub fn issue_dir(out: &Path, id: &str) -> PathBuf {
    out.join(id)
}

pub fn image_path(out: &Path, id: &str, c: &PlanCanvas) -> PathBuf {
    issue_dir(out, id).join(format!("{:04}.{}", c.index, c.ext))
}

/// The collection's manifests in order, each document saved as `_iiif/collections/<n>.json`.
async fn walk(
    client: &Client,
    top: &str,
    terms: Option<&str>,
) -> anyhow::Result<(Vec<String>, Option<u64>, Value)> {
    let dir = client.out.join("_iiif").join("collections");
    let mut queue: VecDeque<String> = VecDeque::from([top.to_owned()]);
    let mut seen: HashSet<String> = HashSet::new();
    let (mut manifests, mut total, mut top_doc) = (Vec::new(), None, Value::Null);
    while let Some(url) = queue.pop_front() {
        if !seen.insert(url.clone()) {
            continue;
        }
        if seen.len() > MAX_COLLECTION_DOCS {
            bail!("more than {MAX_COLLECTION_DOCS} collection documents; stopping");
        }
        let dest = dir.join(format!("{:05}.json", seen.len() - 1));
        let bytes = client
            .doc(&url, &dest, template("collection", terms))
            .await?;
        let doc = json_of(&bytes, &url)?;
        let c = iiif::collection(&doc, &url);
        if seen.len() == 1 {
            total = c.total;
            top_doc = doc;
        }
        manifests.extend(c.manifests);
        // The first page, sub-collections and the next page go in document order, ahead of
        // what follows; any collection document may be paged.
        let follow: Vec<String> = c
            .first
            .into_iter()
            .chain(c.collections)
            .chain(c.next)
            .collect();
        for sub in follow.into_iter().rev() {
            queue.push_front(sub);
        }
    }
    let mut seen_m = HashSet::new();
    manifests.retain(|m| seen_m.insert(m.clone()));
    Ok((manifests, total, top_doc))
}

pub async fn run(opts: Options, stop: watch::Receiver<bool>) -> anyhow::Result<Summary> {
    debug_assert!(opts.action == Action::Plan);
    let top = opts
        .collection
        .clone()
        .context("--collection is required for plan")?;
    let hosts = super::hosts_of(&top, &opts.allow_hosts)?;
    std::fs::create_dir_all(&opts.out)?;
    let client = Client::new(&opts.out, &hosts, opts.pace, stop)?;
    client.check_not_slowed(&top).await?;
    let gap = opts.pace.gap().as_secs_f64();

    // The top document first: its terms are named before anything else is asked.
    let (mut manifests, total, top_doc) = {
        let dest = opts
            .out
            .join("_iiif")
            .join("collections")
            .join("00000.json");
        let bytes = client
            .doc(&top, &dest, template("collection", None))
            .await?;
        let doc = json_of(&bytes, &top)?;
        let terms = doc
            .get("license")
            .map(iiif::text_of)
            .filter(|s| !s.is_empty());
        *client.terms_url.lock().expect("terms") = terms.clone();
        eprintln!("{}", terms_notice(terms.as_deref()));
        walk(&client, &top, terms.as_deref()).await?
    };
    let terms = client.terms_url.lock().expect("terms").clone();
    if let Some(t) = total {
        if t as usize != manifests.len() {
            eprintln!(
                "note: the collection says total {t}, the walk found {} manifests",
                manifests.len()
            );
        }
    }
    if let Some(l) = opts.limit {
        manifests.truncate(l);
    }
    let ids = issue_ids(&manifests);
    let mut issues = Vec::new();
    let mut summary = Summary {
        action: "plan".to_owned(),
        ..Summary::default()
    };
    for (url, id) in manifests.iter().zip(&ids) {
        let dest = opts
            .out
            .join("_iiif")
            .join("manifests")
            .join(format!("{id}.json"));
        let mut t = template("manifest", terms.as_deref());
        t.issue = Some(id.clone());
        let bytes = match client.doc(url, &dest, t).await {
            Ok(b) => b,
            Err(e) => {
                summary.failed.push(format!("{url}: {e:#}"));
                if e.to_string().contains("stopping") || e.to_string() == "stopped" {
                    summary.stopped = Some(format!("{e:#}"));
                    break;
                }
                continue;
            }
        };
        let m = match json_of(&bytes, url).and_then(|v| iiif::manifest(&v, url)) {
            Ok(m) => m,
            Err(e) => {
                summary.failed.push(format!("{url}: {e:#}"));
                continue;
            }
        };
        let mut canvases = Vec::new();
        let mut skipped = Vec::new();
        for c in m.canvases {
            if let Some(why) = &c.skipped {
                skipped.push(format!("canvas {}: {why}", c.index));
                continue;
            }
            let pc = if let Some((service, embedded)) = &c.service {
                let want_info = match opts.info_json {
                    InfoJson::Always => true,
                    InfoJson::Never => false,
                    InfoJson::Auto => embedded.get("profile").is_none(),
                };
                let mut info = embedded.clone();
                if want_info {
                    let info_url = format!("{service}/info.json");
                    let dest = opts
                        .out
                        .join("_iiif")
                        .join("info")
                        .join(id)
                        .join(format!("{:04}.json", c.index));
                    let mut t = template("info", terms.as_deref());
                    t.issue = Some(id.clone());
                    t.canvas_index = Some(c.index);
                    match client.doc(&info_url, &dest, t).await {
                        Ok(b) => info = json_of(&b, &info_url).unwrap_or(info),
                        Err(e) => {
                            let m = format!("{info_url}: {e:#}");
                            if m.contains("stopping") {
                                summary.stopped = Some(m.clone());
                            }
                            summary.failed.push(m);
                            if summary.stopped.is_some() {
                                break;
                            }
                        }
                    }
                }
                let Some(size) = iiif::size(Some(&info), (c.width, c.height), opts.max_dimension)
                else {
                    skipped.push(format!(
                        "canvas {}: the service lists no size within the limit",
                        c.index
                    ));
                    continue;
                };
                PlanCanvas {
                    index: c.index,
                    canvas_id: c.id,
                    label: c.label,
                    url: iiif::image_url(service, &size),
                    service: Some(service.clone()),
                    size,
                    ext: "jpg".to_owned(),
                    width: c.width,
                    height: c.height,
                }
            } else {
                let url = c.resource.clone().unwrap_or_default();
                PlanCanvas {
                    index: c.index,
                    canvas_id: c.id,
                    label: c.label,
                    ext: iiif::extension(c.format.as_deref(), &url),
                    url,
                    service: None,
                    size: "as-given".to_owned(),
                    width: c.width,
                    height: c.height,
                }
            };
            canvases.push(pc);
        }
        if summary.stopped.is_some() {
            break;
        }
        issues.push(PlanIssue {
            id: id.clone(),
            manifest_url: url.clone(),
            manifest_sha256: crate::ghn::sha256_hex(&bytes),
            citation: m.citation,
            canvases,
            skipped,
        });
    }
    let images: usize = issues.iter().map(|i| i.canvases.len()).sum();
    let todo = issues
        .iter()
        .flat_map(|i| i.canvases.iter().map(move |c| (i, c)))
        .filter(|(i, c)| !super::fetch::have_for(&opts.out, i, c).is_verified())
        .count();
    let plan = Plan {
        version: 1,
        created_at: super::now(),
        collection_url: top,
        terms_url: terms,
        attribution: top_doc.get("attribution").cloned(),
        collection_total: total,
        user_agent: crate::loc::USER_AGENT.to_owned(),
        gap_secs: gap,
        issues_count: issues.len(),
        images_count: images,
        estimated_requests: todo,
        estimated_seconds: (todo as f64 * gap).ceil() as u64,
        issues,
    };
    let path = opts
        .plan
        .clone()
        .unwrap_or_else(|| opts.out.join("plan.json"));
    if path.exists() {
        crate::fetcher::set_aside(&path)?;
    }
    crate::ghn::write_atomic(&path, &serde_json::to_vec_pretty(&plan)?)?;
    summary.issues = plan.issues_count;
    summary.images_planned = plan.images_count;
    summary.estimated_requests = plan.estimated_requests;
    summary.estimated_seconds = plan.estimated_seconds;
    eprintln!(
        "plan: {} issues, {} images; about {} s ({:.1} h) at one request every {gap} s; \
         written to {}",
        plan.issues_count,
        plan.images_count,
        plan.estimated_seconds,
        plan.estimated_seconds as f64 / 3600.0,
        path.display()
    );
    Ok(summary)
}
