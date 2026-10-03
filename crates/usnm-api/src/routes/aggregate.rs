//! `GET /v1/aggregate` (06 §6.3.3): totals, national series, per-place totals
//! and first appearance, and the sparse place × bucket cube.

use std::sync::Arc;
use std::time::Instant;

use axum::extract::{OriginalUri, State};
use axum::http::{HeaderMap, Uri};
use axum::response::Response;
use serde::Serialize;
use usnm_core::cube::{Cell, SparseCube};
use usnm_core::params::{RawParams, SearchRequest};
use usnm_core::time::{BucketSpec, BucketUnit};
use usnm_search::plan::{self, Planned};

use super::{cached_body, mount_prefix, uses_fuzzy, with_timeout, Ctx, Job};
use crate::error::ApiError;
use crate::searchlog::{self, Admission, SearchLog};
use crate::{version, AppState};

#[derive(Serialize)]
struct AggregateResponse {
    index_version: String,
    synthetic: bool,
    query: QueryEcho,
    bucket: BucketEcho,
    total: Totals,
    series: Series,
    places: Places,
    cube: CubeOut,
    /// True when buckets were coarsened to keep the cube under the cell cap.
    coarsened: bool,
    timing_ms: Timing,
}

#[derive(Serialize)]
struct QueryEcho {
    canonical: String,
    ast: String,
}

#[derive(Serialize)]
struct BucketEcho {
    unit: BucketUnit,
    from: String,
    to: String,
    count: usize,
}

#[derive(Serialize)]
struct Totals {
    hits: u64,
    places: usize,
    /// Pages published in scope; `null` when filters make the baseline inexact.
    baseline_pages: Option<u64>,
}

#[derive(Serialize)]
struct Series {
    hits: Vec<u64>,
    /// Pages published per bucket; `null` when `lccn` or `front` filters are set,
    /// because baselines are kept per place and day (and per title language), and
    /// when `lang` is set on a version published before baselines were kept per language.
    baseline: Option<Vec<u64>>,
}

#[derive(Serialize)]
struct Places {
    id: Vec<String>,
    hits: Vec<u64>,
    first_day: Vec<u32>,
}

#[derive(Serialize)]
struct CubeOut {
    #[serde(flatten)]
    cells: SparseCube,
    calls: u8,
    baseline_ref: Option<String>,
}

#[derive(Serialize)]
struct Timing {
    backend: u128,
    total: u128,
}

/// A visitor's search, and whether it goes in the search log. Every search
/// on the site requests this endpoint once, so this is where it's counted
/// (06 §6.8).
pub async fn aggregate(
    State(state): State<Arc<AppState>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let ctx = Ctx::serving(&state);
    let log = state
        .search_log
        .as_deref()
        .map(|log| (log, searchlog::admit(&headers, &state.config.site_host)));
    aggregate_in(&state, ctx, &uri, log).await
}

/// `log` is `None` for the warm-up, which is never recorded.
pub(crate) async fn aggregate_in(
    state: &Arc<AppState>,
    ctx: Ctx,
    uri: &Uri,
    log: Option<(&SearchLog, Admission)>,
) -> Result<Response, ApiError> {
    let raw = RawParams::parse(uri.query().unwrap_or(""))?;
    raw.reject_unknown(&["format"])?;
    if raw.get("format").is_some_and(|f| f != "json") {
        return Err(ApiError::Unsupported("format other than json".into()));
    }
    let snap = &ctx.snap;
    let req = SearchRequest::from_raw(&raw, snap.refdata.bounds())?;
    if uses_fuzzy(&req.query) && !snap.backend.capabilities().fuzzy {
        return Err(ApiError::Unsupported(
            "OCR-tolerant (fuzzy) matching".into(),
        ));
    }
    let serving = snap.refdata.version().to_owned();
    let canonical = req.canonical();
    let pinning = match version::check(req.version.as_deref(), &serving, uri.path(), &canonical) {
        Ok(p) => p,
        Err(redirect) => return Ok(*redirect),
    };
    let key = format!("{serving}|aggregate|{canonical}");
    let prefix = mount_prefix(uri.path(), "/aggregate").to_owned();
    let job = Job::search("aggregate", ctx.warm_up, ctx.timeout);
    let search = match log {
        Some((_, Admission::Record)) => Some(searchlog::Search::new(&raw, &req, &serving)),
        _ => None,
    };
    let fut = compute(state.clone(), ctx, req, canonical.clone(), prefix);
    let (resp, body) = cached_body(
        state,
        job,
        key,
        &pinning,
        &serving,
        uri.path(),
        &canonical,
        fut,
    )
    .await?;
    // Still computing (`202`): nothing counts yet. The client asks again, and
    // the request that gets the results is the one that counts.
    let Some(body) = body else {
        return Ok(resp);
    };
    // Only a search that got its results counts, whichever cache served it.
    match (log, search) {
        (Some((log, _)), Some(search)) => {
            log.record(searchlog::Entry::new(chrono::Utc::now(), search, body))
        }
        (Some((log, Admission::Excluded(reason))), _) => log.excluded(reason),
        _ => {}
    }
    Ok(resp)
}

fn coarser(unit: BucketUnit) -> Option<BucketUnit> {
    match unit {
        BucketUnit::Day => Some(BucketUnit::Week),
        BucketUnit::Week => Some(BucketUnit::Month),
        BucketUnit::Month => Some(BucketUnit::Year),
        BucketUnit::Year => None,
    }
}

async fn compute(
    state: Arc<AppState>,
    ctx: Ctx,
    req: SearchRequest,
    canonical: String,
    prefix: String,
) -> Result<AggregateResponse, ApiError> {
    let started = Instant::now();
    let snap = &ctx.snap;
    let rd = &snap.refdata;
    let indexes = rd.index_set();
    let mut spec = req.bucket_spec();
    let mut coarsened = false;
    if let Some((term, delay)) = &state.config.fixture_slow {
        // End-to-end tests: stand in for a cold search (memory backend only).
        if req.query.to_string().to_lowercase().contains(term.as_str()) {
            tokio::time::sleep(*delay).await;
        }
    }
    let t = Instant::now();
    // The planner checks places-with-hits × buckets before issuing the cube,
    // so an oversized cube is coarsened here instead of failing in the engine.
    let agg = async {
        loop {
            let planned = with_timeout(
                &state,
                ctx.timeout,
                plan::aggregate(
                    snap.backend.as_ref(),
                    &indexes,
                    &req.query,
                    &req.filters,
                    &spec,
                    state.config.max_cells,
                ),
            )
            .await?;
            match planned {
                Planned::Complete(agg) => break Ok(agg),
                Planned::TooManyCells { upper_bound } => match coarser(spec.unit) {
                    Some(unit) => {
                        spec = BucketSpec::new(unit, spec.from, spec.to);
                        coarsened = true;
                    }
                    None => {
                        return Err(ApiError::TooBroad(format!(
                            "this search would produce about {upper_bound} map cells even by year"
                        )))
                    }
                },
            }
        }
    }
    .await;
    if !ctx.warm_up {
        state.metrics.backend("aggregate", t.elapsed(), &agg);
    }
    let agg = agg?;
    let backend_ms = t.elapsed().as_millis();

    let mut place_ids: Vec<&str> = agg
        .summary
        .places
        .iter()
        .map(|p| p.place_id.as_str())
        .collect();
    place_ids.sort_unstable();
    let index_of = |id: &str| place_ids.binary_search(&id).ok().map(|i| i as u32);
    let by_id: std::collections::HashMap<&str, &usnm_search::PlaceSummary> = agg
        .summary
        .places
        .iter()
        .map(|p| (p.place_id.as_str(), p))
        .collect();
    let cells: Vec<Cell> = agg
        .cells
        .iter()
        .filter_map(|c| {
            Some(Cell {
                place: index_of(&c.place_id)?,
                bucket: c.bucket,
                hits: c.hits,
            })
        })
        .collect();

    let f = &req.filters;
    let baseline_exact = f.lccns.is_empty() && !f.front_only && rd.has_baselines_for(&f.langs);
    let baseline = baseline_exact.then(|| rd.national_baseline(&spec, &f.states, &f.langs));
    let baseline_ref = baseline_exact.then(|| {
        let mut s = form_urlencoded::Serializer::new(String::new());
        s.append_pair("bucket", spec.unit.as_str());
        s.append_pair("from", &spec.from.to_string());
        if !f.langs.is_empty() {
            s.append_pair("lang", &f.langs.join(","));
        }
        if !f.states.is_empty() {
            s.append_pair("state", &f.states.join(","));
        }
        s.append_pair("to", &spec.to.to_string());
        s.append_pair("v", rd.version());
        // Same mount as this request (`/v1` or `/api/v1`), so the link stays on the API.
        format!("{prefix}/coverage?{}", s.finish())
    });

    Ok(AggregateResponse {
        index_version: rd.version().to_owned(),
        synthetic: rd.current.synthetic,
        query: QueryEcho {
            canonical,
            ast: req.query.to_string(),
        },
        bucket: BucketEcho {
            unit: spec.unit,
            from: spec.from.to_string(),
            to: spec.to.to_string(),
            count: spec.len(),
        },
        total: Totals {
            hits: agg.summary.total_hits,
            places: place_ids.len(),
            baseline_pages: baseline.as_ref().map(|b| b.iter().sum()),
        },
        series: Series {
            hits: agg.summary.series.clone(),
            baseline,
        },
        places: Places {
            hits: place_ids.iter().map(|id| by_id[id].hits).collect(),
            first_day: place_ids.iter().map(|id| by_id[id].first_day).collect(),
            id: place_ids.iter().map(|s| (*s).to_owned()).collect(),
        },
        cube: CubeOut {
            cells: SparseCube::from_cells(cells),
            calls: agg.cube_calls,
            baseline_ref,
        },
        coarsened,
        timing_ms: Timing {
            backend: backend_ms,
            total: started.elapsed().as_millis(),
        },
    })
}
