//! `GET /v1/coverage`: pages published per place and bucket (the "no data" layer
//! and the denominator for per-place relative frequency).

use std::sync::Arc;

use axum::extract::{OriginalUri, State};
use axum::http::Uri;
use axum::response::Response;
use chrono::NaiveDate;
use serde::Serialize;
use usnm_core::cube::{Cell, SparseCube};
use usnm_core::params::{ParamError, RawParams};
use usnm_core::time::{BucketSpec, BucketUnit};

use super::{cached, Ctx, Job};
use crate::error::ApiError;
use crate::{version, AppState};

#[derive(Serialize)]
struct CoverageResponse {
    index_version: String,
    bucket: BucketUnit,
    from: String,
    to: String,
    count: usize,
    places: Vec<String>,
    /// Sparse cube of pages published: (place index, bucket index, pages).
    pages: SparseCube,
}

fn bad(name: &str, reason: &str) -> ApiError {
    ApiError::Params(ParamError::Invalid {
        name: name.to_owned(),
        reason: reason.to_owned(),
    })
}

pub async fn coverage(
    State(state): State<Arc<AppState>>,
    OriginalUri(uri): OriginalUri,
) -> Result<Response, ApiError> {
    let ctx = Ctx::serving(&state);
    coverage_in(&state, ctx, &uri).await
}

pub(crate) async fn coverage_in(
    state: &Arc<AppState>,
    ctx: Ctx,
    uri: &Uri,
) -> Result<Response, ApiError> {
    let raw = RawParams::parse(uri.query().unwrap_or(""))?;
    raw.reject_only(&["from", "to", "bucket", "state", "v"])?;
    let job = Job::reference("coverage", ctx.warm_up, ctx.timeout);
    let snap = ctx.snap;
    let serving = snap.refdata.version().to_owned();
    let (lo, hi) = snap.refdata.bounds();
    let date = |k: &str, d: NaiveDate| {
        raw.get(k).map_or(Ok(d), |v| {
            NaiveDate::parse_from_str(v, "%Y-%m-%d").map_err(|_| bad(k, "must be YYYY-MM-DD"))
        })
    };
    let from = date("from", lo)?.clamp(lo, hi);
    let to = date("to", hi)?.clamp(lo, hi);
    if from > to {
        return Err(bad("from", "must not be after `to`"));
    }
    let unit = match raw.get("bucket") {
        None | Some("auto") => BucketUnit::auto(from, to),
        Some(b) => BucketUnit::parse(b)
            .ok_or_else(|| bad("bucket", "must be auto, year, month, week or day"))?,
    };
    let mut states: Vec<String> = raw
        .get("state")
        .map(|s| {
            s.split(',')
                .map(|x| x.trim().to_ascii_uppercase())
                .collect()
        })
        .unwrap_or_default();
    states.sort();
    states.dedup();
    if states
        .iter()
        .any(|s| s.len() != 2 || !s.bytes().all(|b| b.is_ascii_alphabetic()))
    {
        return Err(bad("state", "must be USPS codes"));
    }

    let canonical = {
        let mut canon = form_urlencoded::Serializer::new(String::new());
        canon
            .append_pair("bucket", unit.as_str())
            .append_pair("from", &from.to_string());
        if !states.is_empty() {
            canon.append_pair("state", &states.join(","));
        }
        canon.append_pair("to", &to.to_string());
        canon.finish()
    };
    let pinning = match version::check(raw.get("v"), &serving, uri.path(), &canonical) {
        Ok(p) => p,
        Err(redirect) => return Ok(*redirect),
    };
    let key = format!("{serving}|coverage|{canonical}");
    let compute = async move {
        let rd = &snap.refdata;
        let spec = BucketSpec::new(unit, from, to);
        let mut places = Vec::new();
        let mut cells = Vec::new();
        for p in rd
            .places
            .iter()
            .filter(|p| states.is_empty() || states.contains(&p.state))
        {
            let series = rd.place_baseline(&p.id, &spec);
            if series.iter().all(|&n| n == 0) {
                continue;
            }
            let idx = places.len() as u32;
            places.push(p.id.clone());
            for (b, &n) in series.iter().enumerate().filter(|(_, n)| **n > 0) {
                cells.push(Cell {
                    place: idx,
                    bucket: b as u32,
                    hits: u32::try_from(n).unwrap_or(u32::MAX),
                });
            }
        }
        Ok::<_, ApiError>(CoverageResponse {
            index_version: rd.version().to_owned(),
            bucket: unit,
            from: from.to_string(),
            to: to.to_string(),
            count: spec.len(),
            places,
            pages: SparseCube::from_cells(cells),
        })
    };
    cached(
        state,
        job,
        key,
        &pinning,
        &serving,
        uri.path(),
        &canonical,
        compute,
    )
    .await
}
