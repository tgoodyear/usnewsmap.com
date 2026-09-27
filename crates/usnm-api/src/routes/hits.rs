//! `GET /v1/hits` (06 §6.3.4): pages for one place or title, by date, with snippets.

use std::sync::Arc;

use axum::extract::{OriginalUri, State};
use axum::response::Response;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use usnm_core::ids::PageKey;
use usnm_core::params::{RawParams, SearchRequest};
use usnm_core::query::highlight_terms;
use usnm_core::time::date_from_day;
use usnm_search::HitsQuery;

use super::{cached, uses_fuzzy, with_timeout};
use crate::error::ApiError;
use crate::{version, AppState};

pub const MAX_LIMIT: usize = 50;
/// Deepest page reachable by offset pagination.
pub const MAX_OFFSET: usize = 10_000;

#[derive(Serialize, Deserialize)]
struct Cursor {
    o: usize,
}

#[derive(Serialize)]
struct HitsResponse {
    index_version: String,
    synthetic: bool,
    place: Option<PlaceOut>,
    title: Option<TitleOut>,
    total: u64,
    items: Vec<Item>,
    next_cursor: Option<String>,
}

#[derive(Serialize)]
struct PlaceOut {
    id: String,
    name: String,
    state: String,
}

#[derive(Serialize)]
struct TitleOut {
    lccn: String,
    name: String,
}

#[derive(Serialize)]
struct Item {
    doc_id: String,
    date: String,
    lccn: String,
    title: Option<String>,
    place_id: String,
    edition: u16,
    seq: u16,
    front_page: bool,
    snippets: Vec<String>,
    links: Links,
}

#[derive(Serialize)]
struct Links {
    viewer: Option<String>,
}

pub async fn hits(
    State(state): State<Arc<AppState>>,
    OriginalUri(uri): OriginalUri,
) -> Result<Response, ApiError> {
    let raw = RawParams::parse(uri.query().unwrap_or(""))?;
    raw.reject_unknown(&["place", "title", "cursor", "limit"])?;
    let rd = state.refdata.load_full();
    let req = SearchRequest::from_raw(&raw, rd.bounds())?;
    if uses_fuzzy(&req.query) && !state.backend.capabilities().fuzzy {
        return Err(ApiError::Unsupported(
            "OCR-tolerant (fuzzy) matching".into(),
        ));
    }
    let place = raw.get("place").map(str::to_owned);
    let title = raw.get("title").map(str::to_owned);
    match (&place, &title) {
        (Some(_), Some(_)) | (None, None) => {
            return Err(ApiError::BadRequest(
                "give exactly one of `place` or `title`".into(),
            ))
        }
        (Some(p), None) if rd.place(p).is_none() => {
            return Err(ApiError::NotFound(format!("unknown place `{p}`")))
        }
        (None, Some(t)) if !rd.titles.contains_key(t) => {
            return Err(ApiError::NotFound(format!("unknown title `{t}`")))
        }
        _ => {}
    }
    let limit = match raw.get("limit") {
        None => MAX_LIMIT,
        Some(l) => l
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=MAX_LIMIT).contains(n))
            .ok_or_else(|| ApiError::BadRequest(format!("`limit` must be 1–{MAX_LIMIT}")))?,
    };
    let offset = match raw.get("cursor") {
        None => 0,
        Some(c) => URL_SAFE_NO_PAD
            .decode(c)
            .ok()
            .and_then(|b| serde_json::from_slice::<Cursor>(&b).ok())
            .map(|c| c.o)
            .filter(|o| *o <= MAX_OFFSET)
            .ok_or_else(|| ApiError::BadRequest("invalid `cursor`".into()))?,
    };

    let canonical = {
        let mut canon = form_urlencoded::Serializer::new(req.canonical());
        if let Some(c) = raw.get("cursor") {
            canon.append_pair("cursor", c);
        }
        canon.append_pair("limit", &limit.to_string());
        if let Some(p) = &place {
            canon.append_pair("place", p);
        }
        if let Some(t) = &title {
            canon.append_pair("title", t);
        }
        canon.finish()
    };
    let pinning = match version::check(req.version.as_deref(), rd.version(), uri.path(), &canonical)
    {
        Ok(p) => p,
        Err(redirect) => return Ok(*redirect),
    };
    let key = format!("{}|hits|{canonical}", rd.version());
    let st = state.clone();
    let rd2 = rd.clone();
    let compute = async move {
        let page = HitsQuery {
            place_id: place.clone(),
            lccn: title.clone(),
            offset,
            limit,
        };
        let result = with_timeout(
            &st,
            st.backend
                .hits(&rd2.index_set(), &req.query, &req.filters, &page),
        )
        .await?;
        let highlight = highlight_terms(&req.query).join(" ");
        let next = offset + result.hits.len();
        let next_cursor = (next < result.total as usize && next <= MAX_OFFSET).then(|| {
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&Cursor { o: next }).unwrap_or_default())
        });
        Ok::<_, ApiError>(HitsResponse {
            index_version: rd2.version().to_owned(),
            synthetic: rd2.current.synthetic,
            place: place
                .as_deref()
                .and_then(|p| rd2.place(p))
                .map(|p| PlaceOut {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    state: p.state.clone(),
                }),
            title: title
                .as_deref()
                .and_then(|t| rd2.titles.get(t))
                .map(|t| TitleOut {
                    lccn: t.lccn.clone(),
                    name: t.name.clone(),
                }),
            total: result.total,
            items: result
                .hits
                .into_iter()
                .map(|h| Item {
                    date: date_from_day(h.day).to_string(),
                    title: rd2.titles.get(&h.lccn).map(|t| t.name.clone()),
                    links: Links {
                        viewer: PageKey::from_doc_id(&h.doc_id)
                            .ok()
                            .map(|k| k.viewer_url(Some(&highlight))),
                    },
                    doc_id: h.doc_id,
                    lccn: h.lccn,
                    place_id: h.place_id,
                    edition: h.edition,
                    seq: h.seq,
                    front_page: h.front_page,
                    snippets: h.snippets,
                })
                .collect(),
            next_cursor,
        })
    };
    cached(
        &state,
        key,
        &pinning,
        rd.version(),
        uri.path(),
        &canonical,
        compute,
    )
    .await
}
