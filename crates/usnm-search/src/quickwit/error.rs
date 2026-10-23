//! Quickwit's failures as search errors, and what each was about without its text (09 §9.4.2).

use crate::SearchError;

pub(super) fn map_err(e: reqwest::Error) -> SearchError {
    if e.is_timeout() {
        return SearchError::Timeout;
    }
    // The whole chain (e.g. "connection refused" or "connection closed before
    // message completed"), which says whether the sidecar went away. It holds
    // the URL (index names) and transport errors, never the query, which is
    // in the request body.
    let mut msg = e.to_string();
    let mut source = std::error::Error::source(&e);
    while let Some(s) = source {
        msg.push_str(": ");
        msg.push_str(&s.to_string());
        source = s.source();
    }
    let kind = if e.is_connect() {
        "connect"
    } else if e.is_decode() {
        "decode"
    } else if e.is_body() {
        "body"
    } else {
        "request"
    };
    SearchError::Backend(format!("quickwit {kind} error: {msg}"))
}

/// What a Quickwit error message is about, by keyword, so its cause can be
/// logged without its text (which may echo the query, 09 §9.4.2). The
/// message is compared word by word, leaving out the words of `query` (the
/// Quickwit query string sent), so an echoed query can't decide the
/// category: a search for "memory" that fails on storage is `storage`. A
/// failure whose only telling word is also in the query is `other`.
pub fn cause(message: &str, query: &str) -> &'static str {
    let echoed: std::collections::HashSet<String> = words(query).collect();
    let w: Vec<String> = words(message).filter(|w| !echoed.contains(w)).collect();
    let any = |of: &[&str]| w.iter().any(|x| of.contains(&x.as_str()));
    let pair = |a: &str, b: &str| w.windows(2).any(|p| p[0] == a && p[1] == b);
    if any(&["memory"]) {
        "memory_limit"
    } else if any(&["bucket", "buckets"]) {
        "bucket_limit"
    } else if any(&["timeout", "deadline", "elapsed"]) || pair("timed", "out") {
        "timeout"
    } else if pair("too", "many")
        || pair("rate", "limit")
        || any(&[
            "concurrent",
            "concurrency",
            "throttled",
            "throttling",
            "503",
            "429",
        ])
    {
        "overloaded"
    } else if any(&[
        "storage",
        "azure",
        "blob",
        "io",
        "connection",
        "http",
        "network",
    ]) {
        "storage"
    } else if any(&[
        "split",
        "splits",
        "footer",
        "hotcache",
        "corrupt",
        "corrupted",
    ]) {
        "split"
    } else if w.is_empty() {
        "empty"
    } else {
        "other"
    }
}

/// `; splits A, B` for up to 5 split ids named in a Quickwit message, or
/// nothing. Split ids are ULIDs: 26 characters of Crockford base32 in upper
/// case, starting 0-7, with digits and letters. A word of `query` (the
/// query string sent, which the message may echo) is never one, in any
/// case, so no search text is logged.
pub(super) fn splits_named(message: &str, query: &str) -> String {
    let echoed: std::collections::HashSet<String> = words(query).collect();
    let crockford = |c: char| c.is_ascii_digit() || (c.is_ascii_uppercase() && !"ILOU".contains(c));
    let mut ids: Vec<&str> = Vec::new();
    for w in message.split(|c: char| !c.is_ascii_alphanumeric()) {
        if w.len() == 26
            && w.starts_with(|c: char| ('0'..='7').contains(&c))
            && w.chars().all(crockford)
            && !echoed.contains(&w.to_ascii_lowercase())
            && w.chars().any(|c| c.is_ascii_digit())
            && w.chars().any(|c| c.is_ascii_uppercase())
            && !ids.contains(&w)
        {
            ids.push(w);
        }
    }
    if ids.is_empty() {
        String::new()
    } else {
        format!("; splits {}", ids[..ids.len().min(5)].join(", "))
    }
}

/// Lowercase ASCII alphanumeric words.
fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quickwit::{check_complete, SearchResponse};
    use serde_json::json;

    #[test]
    fn rejects_partial_failures_without_echoing_them() {
        let resp: SearchResponse = serde_json::from_value(json!({
            "num_hits": 3,
            "errors": [{"split_id": "x", "error": "failed on query text:secret"}]
        }))
        .unwrap();
        let err = check_complete(&resp, "text:secret")
            .unwrap_err()
            .to_string();
        assert!(err.contains("1 partial failure"));
        assert!(!err.contains("secret"));
    }

    #[test]
    fn partial_failures_say_what_they_were_about() {
        let resp: SearchResponse = serde_json::from_value(json!({
            "num_hits": 3,
            "errors": [
                "split 01JA9XQ3V5Z8M2K7T4R6N1P0WB: aggregation memory limit exceeded on query text:secret",
                "split 01JA9XQ3V5Z8M2K7T4R6N1P0WC: Aggregation Memory Limit exceeded",
                "request timed out for text:SECRETSECRETSECRETSECRET12",
                "storage error: Azure blob read failed for text:secret"
            ]
        }))
        .unwrap();
        let err = check_complete(&resp, "text:secret OR text:SECRETSECRETSECRETSECRET12")
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            "search backend error: quickwit reported 4 partial failure(s): \
             memory_limit×2, storage×1, timeout×1; \
             splits 01JA9XQ3V5Z8M2K7T4R6N1P0WB, 01JA9XQ3V5Z8M2K7T4R6N1P0WC"
        );
        assert!(!err.to_lowercase().contains("secret"), "{err}");
    }

    #[test]
    fn split_ids_are_named_only_when_they_look_like_ulids() {
        assert_eq!(
            splits_named(
                "failed splits: [01JA9XQ3V5Z8M2K7T4R6N1P0WB, 01JA9XQ3V5Z8M2K7T4R6N1P0WB]",
                ""
            ),
            "; splits 01JA9XQ3V5Z8M2K7T4R6N1P0WB"
        );
        // Too short, lower case, a letter Crockford leaves out, or not 0-7 first.
        assert_eq!(
            splits_named(
                "01JA9X 01ja9xq3v5z8m2k7t4r6n1p0wb 01JA9XQ3V5Z8M2K7T4R6N1P0WI 91JA9XQ3V5Z8M2K7T4R6N1P0WB",
                ""
            ),
            ""
        );
        // A word of the query is never named, whatever its case.
        assert_eq!(
            splits_named(
                "no match for 01JA9XQ3V5Z8M2K7T4R6N1P0WB",
                "text:01ja9xq3v5z8m2k7t4r6n1p0wb"
            ),
            ""
        );
    }

    #[test]
    fn causes_are_classified_by_keyword() {
        assert_eq!(
            cause("Aggregation memory limit of 768MB exceeded", ""),
            "memory_limit"
        );
        assert_eq!(
            cause("too many buckets: 210000 > 200000", ""),
            "bucket_limit"
        );
        assert_eq!(cause("deadline exceeded", ""), "timeout");
        assert_eq!(cause("Too many requests", ""), "overloaded");
        assert_eq!(cause("failed to fetch footer", ""), "split");
        assert_eq!(cause("Io error: broken pipe", ""), "storage");
        assert_eq!(cause("", ""), "empty");
        assert_eq!(cause("something else", ""), "other");
        assert_eq!(cause("timed out after 30s", ""), "timeout");
    }

    #[test]
    fn an_echoed_query_does_not_decide_the_cause() {
        // A search for "memory" that failed on storage is a storage failure.
        assert_eq!(
            cause(
                "Azure blob read failed for query text:memory",
                "text:memory"
            ),
            "storage"
        );
        assert_eq!(
            cause("failed for text:\"bucket http\"", "text:\"bucket http\""),
            "other"
        );
        // Words of the query that aren't echoed still count.
        assert_eq!(
            cause("aggregation memory limit exceeded", "text:gold"),
            "memory_limit"
        );
    }
}
