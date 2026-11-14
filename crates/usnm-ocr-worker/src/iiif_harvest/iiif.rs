//! IIIF Presentation 2.x and Image 2.x documents, read as the specifications describe them:
//! `@id`s are followed (relative ones against the document's own URL), never built from a
//! pattern. A collection page lists `manifests`, `members` and `collections` and points on
//! with `next`; a manifest's canvases carry images whose `resource.service` is an image
//! service, or just a `resource.@id`.

use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::Value;

fn id_of(v: &Value) -> Option<&str> {
    match v {
        Value::String(s) => Some(s),
        Value::Object(o) => o.get("@id").or_else(|| o.get("id")).and_then(Value::as_str),
        _ => None,
    }
}

fn join(base: &str, link: &str) -> Option<String> {
    Url::parse(base).ok()?.join(link).ok().map(Into::into)
}

/// What a collection document points at.
#[derive(Debug, Default, PartialEq)]
pub struct Collection {
    pub manifests: Vec<String>,
    pub collections: Vec<String>,
    /// `first`, to start a paged collection.
    pub first: Option<String>,
    pub next: Option<String>,
    pub total: Option<u64>,
}

pub fn collection(doc: &Value, base: &str) -> Collection {
    let mut c = Collection {
        total: doc.get("total").and_then(Value::as_u64),
        ..Collection::default()
    };
    let abs = |v: &Value| id_of(v).and_then(|l| join(base, l));
    for key in ["manifests", "collections", "members"] {
        for item in doc.get(key).and_then(Value::as_array).into_iter().flatten() {
            let Some(url) = abs(item) else { continue };
            let kind = item.get("@type").or_else(|| item.get("type"));
            let is_collection = match kind.and_then(Value::as_str) {
                Some(k) => k.ends_with("Collection"),
                None => key == "collections",
            };
            if is_collection {
                c.collections.push(url);
            } else {
                c.manifests.push(url);
            }
        }
    }
    c.first = doc.get("first").and_then(&abs);
    c.next = doc.get("next").and_then(&abs);
    c
}

/// What a citation needs, as the manifest gives it. Nothing is built from a URL pattern.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Citation {
    pub manifest_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub related: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub see_also: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rendering: Option<Value>,
    /// Links the manifest gives (`related`, `seeAlso`, `rendering`, metadata values that are
    /// URLs), as written.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<String>,
}

impl Citation {
    /// The persistent link if the manifest gave one, otherwise the manifest `@id`.
    pub fn link_or_id(&self) -> &str {
        self.links.first().map_or(&self.manifest_id, String::as_str)
    }

    /// The label as plain text (a string, or the first `@value` of a language map).
    pub fn label_text(&self) -> String {
        self.label.as_ref().map(text_of).unwrap_or_default()
    }

    pub fn attribution_text(&self) -> String {
        self.attribution.as_ref().map(text_of).unwrap_or_default()
    }
}

pub fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.first().map(text_of).unwrap_or_default(),
        Value::Object(o) => o
            .get("@value")
            .or_else(|| o.get("value"))
            .map(text_of)
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn links_in(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Array(a) => a.iter().for_each(|x| links_in(x, out)),
        other => {
            if let Some(l) = id_of(other).filter(|l| l.starts_with("http")) {
                out.push(l.to_owned());
            }
        }
    }
}

/// One canvas of a manifest.
#[derive(Debug, Clone, PartialEq)]
pub struct Canvas {
    /// 1-based position in the manifest.
    pub index: usize,
    pub id: String,
    pub label: Option<String>,
    pub width: Option<u64>,
    pub height: Option<u64>,
    /// The image service's base URL and the service object as the manifest embeds it.
    pub service: Option<(String, Value)>,
    /// `resource.@id`, the fallback when there is no service.
    pub resource: Option<String>,
    pub format: Option<String>,
    /// Why the canvas has no image.
    pub skipped: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    pub citation: Citation,
    pub canvases: Vec<Canvas>,
}

pub fn manifest(doc: &Value, url: &str) -> anyhow::Result<Manifest> {
    let kind = doc.get("@type").and_then(Value::as_str).unwrap_or_default();
    anyhow::ensure!(
        kind.is_empty() || kind.ends_with("Manifest"),
        "{url}: not a manifest ({kind})"
    );
    let id = id_of(doc)
        .and_then(|l| join(url, l))
        .unwrap_or_else(|| url.to_owned());
    let mut cite = Citation {
        manifest_id: id.clone(),
        label: doc.get("label").cloned(),
        attribution: doc.get("attribution").cloned(),
        license: doc.get("license").cloned(),
        metadata: doc.get("metadata").cloned(),
        related: doc.get("related").cloned(),
        see_also: doc.get("seeAlso").cloned(),
        rendering: doc.get("rendering").cloned(),
        links: Vec::new(),
    };
    for key in ["related", "seeAlso", "rendering"] {
        if let Some(v) = doc.get(key) {
            links_in(v, &mut cite.links);
        }
    }
    for m in doc
        .get("metadata")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let label = m
            .get("label")
            .map(text_of)
            .unwrap_or_default()
            .to_lowercase();
        let named_link = ["link", "url", "identifier", "permalink", "ark", "uri"]
            .iter()
            .any(|k| label.contains(k));
        let value = m.get("value").map(text_of).unwrap_or_default();
        if named_link && value.starts_with("http") {
            cite.links.push(value);
        }
    }
    cite.links.dedup();
    let sequences = doc
        .get("sequences")
        .and_then(Value::as_array)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("{url}: no sequences"))?;
    let mut canvases = Vec::new();
    for canvas in sequences.iter().flat_map(|s| {
        s.get("canvases")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
    }) {
        let index = canvases.len() + 1;
        let mut c = Canvas {
            index,
            id: id_of(canvas).unwrap_or_default().to_owned(),
            label: canvas.get("label").map(text_of).filter(|l| !l.is_empty()),
            width: canvas.get("width").and_then(Value::as_u64),
            height: canvas.get("height").and_then(Value::as_u64),
            service: None,
            resource: None,
            format: None,
            skipped: None,
        };
        let res = canvas
            .get("images")
            .and_then(Value::as_array)
            .and_then(|a| a.iter().find_map(|i| i.get("resource")));
        match res {
            None => c.skipped = Some("no image on the canvas".to_owned()),
            Some(res) => {
                c.format = res.get("format").and_then(Value::as_str).map(str::to_owned);
                c.width = c.width.or_else(|| res.get("width").and_then(Value::as_u64));
                c.height = c
                    .height
                    .or_else(|| res.get("height").and_then(Value::as_u64));
                let service = match res.get("service") {
                    Some(Value::Array(a)) => a.iter().find(|s| id_of(s).is_some()),
                    Some(s) if id_of(s).is_some() => Some(s),
                    _ => None,
                };
                if let Some((s, sid)) = service.and_then(|s| Some((s, join(url, id_of(s)?)?))) {
                    c.service = Some((sid.trim_end_matches('/').to_owned(), s.clone()));
                } else if let Some(r) = id_of(res).and_then(|r| join(url, r)) {
                    c.resource = Some(r);
                } else {
                    c.skipped = Some("the image has neither a service nor an @id".to_owned());
                }
            }
        }
        canvases.push(c);
    }
    Ok(Manifest {
        citation: cite,
        canvases,
    })
}

/// The `size` segment for the largest image the service allows, within `max_dimension` on
/// the longer side if asked. `info` is the service's `info.json` or the service object the
/// manifest embeds. Without dimensions the answer is `full`.
pub fn size(
    info: Option<&Value>,
    dims: (Option<u64>, Option<u64>),
    max_dimension: Option<u64>,
) -> String {
    let get = |k: &str| info.and_then(|i| i.get(k)).and_then(Value::as_u64);
    let (Some(w), Some(h)) = (get("width").or(dims.0), get("height").or(dims.1)) else {
        return "full".to_owned();
    };
    let (wf, hf) = (w as f64, h as f64);
    let (mut max_w, mut max_h, mut max_a, mut level0) = (None, None, None, false);
    let profile = info.and_then(|i| i.get("profile"));
    let list: Vec<&Value> = match profile {
        Some(Value::Array(a)) => a.iter().collect(),
        Some(p) => vec![p],
        None => Vec::new(),
    };
    for p in list {
        if let Some(s) = p.as_str() {
            level0 |= s.ends_with("level0.json") || s.ends_with("/level0");
        }
        max_w = max_w.or_else(|| p.get("maxWidth").and_then(Value::as_u64));
        max_h = max_h.or_else(|| p.get("maxHeight").and_then(Value::as_u64));
        max_a = max_a.or_else(|| p.get("maxArea").and_then(Value::as_u64));
    }
    let mut s = 1.0f64;
    if let Some(mw) = max_w {
        s = s.min(mw as f64 / wf);
        s = s.min(max_h.unwrap_or(mw) as f64 / hf);
    }
    if let Some(ma) = max_a {
        s = s.min((ma as f64 / (wf * hf)).sqrt());
    }
    if let Some(d) = max_dimension {
        s = s.min(d as f64 / wf.max(hf));
    }
    if s >= 1.0 {
        return "full".to_owned();
    }
    if level0 {
        let best = info
            .and_then(|i| i.get("sizes"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|z| Some((z.get("width")?.as_u64()?, z.get("height")?.as_u64()?)))
            .filter(|(zw, _)| (*zw as f64) <= wf * s + 0.5)
            .max_by_key(|(zw, _)| *zw);
        return best.map_or_else(|| "full".to_owned(), |(zw, zh)| format!("{zw},{zh}"));
    }
    format!("{},", ((wf * s).floor() as u64).max(1))
}

/// `<service>/full/<size>/0/default.jpg`.
pub fn image_url(service: &str, size: &str) -> String {
    format!(
        "{}/full/{size}/0/default.jpg",
        service.trim_end_matches('/')
    )
}

/// A file extension for a fallback resource: its `format`, else its URL's, else `jpg`.
pub fn extension(format: Option<&str>, url: &str) -> String {
    let by_mime = match format {
        Some("image/jpeg") => Some("jpg"),
        Some("image/png") => Some("png"),
        Some("image/tiff") => Some("tif"),
        Some("image/jp2") => Some("jp2"),
        Some("image/gif") => Some("gif"),
        _ => None,
    };
    if let Some(e) = by_mime {
        return e.to_owned();
    }
    let path = Url::parse(url)
        .map(|u| u.path().to_owned())
        .unwrap_or_default();
    path.rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .filter(|e| (1..=4).contains(&e.len()) && e.bytes().all(|b| b.is_ascii_alphanumeric()))
        .unwrap_or_else(|| "jpg".to_owned())
}
