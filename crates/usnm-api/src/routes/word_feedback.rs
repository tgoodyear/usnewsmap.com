//! `POST /v1/word-feedback`: a reader's verdict on one word of a page view
//! (06 §6.3.13, #2026).
//!
//! The body is `{v, doc_id, word_id, verdict}` and nothing else: unknown
//! fields are refused and there is no free text. The page and word ids are
//! checked for shape only; the ALTO is not read on this path, so a mark for a
//! word that doesn't exist costs no more than a valid one. The mark is stored
//! as [`crate::ocr_requests`] stores page ids (see [`crate::word_feedback`]):
//! no address, user agent or time beyond the UTC day.
//!
//! A request whose `Origin` is another site is refused with 403; one with no
//! `Origin` is accepted, as for `/v1/beacon`.
//!
//! `DNT` and `Sec-GPC` are not read. They opt a visitor out of tracking and
//! analytics, and a mark carries no identity; the reader marks a word on
//! purpose. Declining to mark is the opt-out.
//!
//! With no store (`USNM_WORD_FEEDBACK_URL` unset) the route answers 503, not
//! 204: a search's page-id queue is a side effect nobody sees, but here the
//! mark is the whole request, and the web app keeps the reader's local mark
//! and says the mark wasn't sent only if the server says so.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde::Deserialize;
use usnm_core::ids::PageKey;
use usnm_core::page_id::{PageId, Source};

use super::beacon::{is_other_origin, read_body};
use crate::error::ApiError;
use crate::word_feedback::{Mark, Verdict};
use crate::AppState;

/// Largest body accepted, in bytes: a page id, a word id and a verdict.
pub const MAX_BODY: usize = 512;
/// Longest word id, in bytes: a stamp, three counters and the colons.
pub const MAX_WORD_ID: usize = 64;
/// Longest ALTO stamp, in bytes.
const MAX_STAMP: usize = 32;
/// Most digits in a block, line or word index.
const MAX_INDEX_DIGITS: usize = 7;
/// The only version of the body.
const VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    v: u32,
    doc_id: String,
    word_id: String,
    verdict: Verdict,
}

pub async fn word_feedback(State(state): State<Arc<AppState>>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    // text/plain is CORS-safelisted, so another site's page can post a mark
    // from a visitor's browser without a preflight. Browsers always send
    // Origin on a cross-site POST; refuse it before reading the body. A
    // request with no Origin (a script, curl) is accepted, as `/v1/beacon` does.
    if is_other_origin(&state, &parts.headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let bytes = match read_body(&parts.headers, body, "the word mark", MAX_BODY).await {
        Ok(bytes) => bytes,
        Err(e) => return e.into_response(),
    };
    let mark = match parse(&bytes) {
        Ok(mark) => mark,
        Err(e) => return e.into_response(),
    };
    let Some(feedback) = &state.word_feedback else {
        return ApiError::Unavailable("word feedback is not enabled on this server".to_owned())
            .into_response();
    };
    if !feedback.record(Utc::now().date_naive(), mark) {
        return ApiError::Unavailable("word feedback is busy; try again later".to_owned())
            .into_response();
    }
    let mut resp = StatusCode::NO_CONTENT.into_response();
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

fn bad(msg: &str) -> ApiError {
    ApiError::BadRequest(msg.to_owned())
}

/// A mark from the request body, or why it isn't one.
fn parse(bytes: &[u8]) -> Result<Mark, ApiError> {
    // Only an object: serde would also read a struct from an array.
    if bytes.trim_ascii_start().first() != Some(&b'{') {
        return Err(bad(
            "the body must be a JSON object with v, doc_id, word_id and verdict",
        ));
    }
    let body: Body = serde_json::from_slice(bytes).map_err(|_| {
        bad("the body must be a JSON object with v, doc_id, word_id (right or wrong) and verdict, and no other field")
    })?;
    if body.v != VERSION {
        return Err(bad("v must be 1"));
    }
    let doc_id = doc_id(&body.doc_id)?;
    if !word_id_ok(&body.word_id) {
        return Err(bad(
            "word_id must be {stamp}:{block}:{line}:{word}, as the layout gives it",
        ));
    }
    Ok(Mark::new(doc_id, body.word_id, body.verdict))
}

/// The bare LoC page id of `raw`, bare or with the `loc:` prefix.
fn doc_id(raw: &str) -> Result<String, ApiError> {
    let id: PageId = raw
        .parse()
        .map_err(|_| bad("doc_id must be a LoC page id, bare or with the loc: prefix"))?;
    if id.source() != Source::Loc || PageKey::from_doc_id(id.local()).is_err() {
        return Err(bad(
            "doc_id must be a LoC page id, bare or with the loc: prefix",
        ));
    }
    Ok(id.local().to_owned())
}

/// `{stamp}:{b}:{l}:{i}`: a stamp of letters, digits, `-` and `_`, then three
/// decimal indexes without leading zeros.
fn word_id_ok(id: &str) -> bool {
    if id.len() > MAX_WORD_ID {
        return false;
    }
    let mut parts = id.split(':');
    let (Some(stamp), Some(b), Some(l), Some(i), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return false;
    };
    let stamp_ok = !stamp.is_empty()
        && stamp.len() <= MAX_STAMP
        && stamp
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'));
    let index_ok = |s: &str| {
        !s.is_empty()
            && s.len() <= MAX_INDEX_DIGITS
            && s.bytes().all(|c| c.is_ascii_digit())
            && (s == "0" || !s.starts_with('0'))
    };
    stamp_ok && index_ok(b) && index_ok(l) && index_ok(i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_ids_have_a_stamp_and_three_plain_indexes() {
        for ok in ["20261102T000000Z:0:0:0", "s-1_a:12:345:6789"] {
            assert!(word_id_ok(ok), "{ok}");
        }
        for bad in [
            "",
            "20261102T000000Z:0:0",
            "20261102T000000Z:0:0:0:0",
            ":0:0:0",
            "a b:0:0:0",
            "a.b:0:0:0",
            "a:01:0:0",
            "a:-1:0:0",
            "a:0:0:",
            "a:0:0:12345678",
            "a:0:0:x",
            &format!("{}:0:0:0", "a".repeat(33)),
        ] {
            assert!(!word_id_ok(bad), "{bad}");
        }
    }

    #[test]
    fn page_ids_are_loc_bare_or_qualified() {
        let bare = "sn85055101_1917-04-03_ed-1_seq-3";
        assert_eq!(doc_id(bare).unwrap(), bare);
        assert_eq!(doc_id(&format!("loc:{bare}")).unwrap(), bare);
        for bad in [
            "",
            "ghn:sn85055101_1917-04-03_ed-1_seq-3",
            "nope",
            "loc:nope",
        ] {
            assert!(doc_id(bad).is_err(), "{bad}");
        }
    }
}
