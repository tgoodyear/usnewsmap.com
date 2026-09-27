//! `v` validation and cache headers (06 §6.5).
//!
//! - `v` equals the serving version → cacheable for a day, with an ETag.
//! - `v` missing → served, cacheable for 5 minutes, `Content-Location` names the versioned URL.
//! - `v` differs → 307 to the same canonical query at the serving version, `no-store`.

use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};

pub enum Pinning {
    Current,
    Unversioned,
}

/// Returns `Err(redirect)` when the client asked for a different version.
pub fn check(
    requested: Option<&str>,
    serving: &str,
    path: &str,
    canonical: &str,
) -> Result<Pinning, Box<Response>> {
    match requested {
        None => Ok(Pinning::Unversioned),
        Some(v) if v == serving => Ok(Pinning::Current),
        Some(_) => {
            let location = format!("{path}?{}", with_version(canonical, serving));
            let mut resp = StatusCode::TEMPORARY_REDIRECT.into_response();
            let h = resp.headers_mut();
            h.insert(
                header::LOCATION,
                HeaderValue::from_str(&location).expect("ascii url"),
            );
            h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            Err(Box::new(resp))
        }
    }
}

pub fn with_version(canonical: &str, version: &str) -> String {
    let mut s = form_urlencoded::Serializer::new(canonical.to_owned());
    s.append_pair("v", version);
    s.finish()
}

pub fn cache_headers(
    pinning: &Pinning,
    serving: &str,
    path: &str,
    canonical: &str,
    body: &[u8],
) -> HeaderMap {
    let mut h = HeaderMap::new();
    match pinning {
        Pinning::Current => {
            h.insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=86400"),
            );
            let digest = Sha256::digest(body);
            let tag = format!(
                "\"{serving}:{:x}\"",
                u64::from_be_bytes(digest[..8].try_into().expect("8 bytes"))
            );
            if let Ok(v) = HeaderValue::from_str(&tag) {
                h.insert(header::ETAG, v);
            }
        }
        Pinning::Unversioned => {
            h.insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=300"),
            );
            let loc = format!("{path}?{}", with_version(canonical, serving));
            if let Ok(v) = HeaderValue::from_str(&loc) {
                h.insert(header::CONTENT_LOCATION, v);
            }
        }
    }
    h
}
