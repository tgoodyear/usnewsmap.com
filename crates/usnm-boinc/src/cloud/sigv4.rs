//! A minimal AWS Signature Version 4 request signer, enough for one single-part object PUT to R2
//! (region `auto`, service `s3`) with an unsigned payload: the body streams from a file, so its
//! SHA-256 is never taken (`x-amz-content-sha256: UNSIGNED-PAYLOAD`).
use super::hmac::{hex, hmac_sha256};
use sha2::{Digest, Sha256};

pub(super) const UNSIGNED_PAYLOAD: &str = "UNSIGNED-PAYLOAD";

/// The key pair of an R2 API token. Never printed: it has no `Debug`.
pub(super) struct Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
}

/// A request to sign: the method, the unencoded path, the query (empty for a PUT here) and the
/// headers to sign, with lowercase names. `host`, `x-amz-date` and `x-amz-content-sha256` must
/// be among them.
pub(super) struct Request<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub query: &'a [(&'a str, &'a str)],
    pub headers: &'a [(&'a str, &'a str)],
    pub payload_hash: &'a str,
}

/// Percent-encode as SigV4 wants: everything but `A-Za-z0-9-_.~`, and `/` too unless `keep_slash`.
fn encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else if b == b'/' && keep_slash {
            out.push('/');
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The signed header names and the canonical request.
fn canonical_request(r: &Request) -> (String, String) {
    let mut headers: Vec<(String, String)> = r
        .headers
        .iter()
        .map(|(k, v)| {
            (
                k.to_ascii_lowercase(),
                v.split_whitespace().collect::<Vec<_>>().join(" "),
            )
        })
        .collect();
    headers.sort();
    let mut query: Vec<(String, String)> = r
        .query
        .iter()
        .map(|(k, v)| (encode(k, false), encode(v, false)))
        .collect();
    query.sort();
    let names = headers
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let canonical = format!(
        "{}\n{}\n{}\n{}\n\n{}\n{}",
        r.method,
        encode(r.path, true),
        query
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&"),
        headers
            .iter()
            .map(|(k, v)| format!("{k}:{v}"))
            .collect::<Vec<_>>()
            .join("\n"),
        names,
        r.payload_hash
    );
    (names, canonical)
}

fn signing_key(secret: &str, date: &str, region: &str, service: &str) -> [u8; 32] {
    let k = hmac_sha256(format!("AWS4{secret}").as_bytes(), &[date.as_bytes()]);
    let k = hmac_sha256(&k, &[region.as_bytes()]);
    let k = hmac_sha256(&k, &[service.as_bytes()]);
    hmac_sha256(&k, &[b"aws4_request"])
}

/// The `Authorization` header value. `amz_date` is `YYYYMMDDTHHMMSSZ`.
pub(super) fn authorization(
    creds: &Credentials,
    region: &str,
    service: &str,
    amz_date: &str,
    r: &Request,
) -> String {
    let (names, canonical) = canonical_request(r);
    let date = &amz_date[..8];
    let scope = format!("{date}/{region}/{service}/aws4_request");
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex(&Sha256::digest(canonical.as_bytes()))
    );
    let key = signing_key(&creds.secret_access_key, date, region, service);
    let signature = hex(&hmac_sha256(&key, &[to_sign.as_bytes()]));
    format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={names}, Signature={signature}",
        creds.access_key_id
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example_credentials() -> Credentials {
        Credentials {
            access_key_id: "AKIAIOSFODNN7EXAMPLE".into(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
        }
    }

    /// "Deriving a signing key" in the AWS SigV4 documentation (the IAM example).
    #[test]
    fn the_signing_key_matches_the_documented_derivation() {
        let key = signing_key(
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            "20150830",
            "us-east-1",
            "iam",
        );
        assert_eq!(
            hex(&key),
            "c4afb1cc5771d871763a393e44b703571b55cc28424d1a5e86da6ed3c154a4b9"
        );
    }

    /// S3 "Example: GET Object" with a Range header, from the AWS S3 SigV4 documentation.
    #[test]
    fn get_object_with_a_range_matches_the_aws_example() {
        let empty = hex(&Sha256::digest(b""));
        let auth = authorization(
            &example_credentials(),
            "us-east-1",
            "s3",
            "20130524T000000Z",
            &Request {
                method: "GET",
                path: "/test.txt",
                query: &[],
                headers: &[
                    ("host", "examplebucket.s3.amazonaws.com"),
                    ("range", "bytes=0-9"),
                    ("x-amz-content-sha256", &empty),
                    ("x-amz-date", "20130524T000000Z"),
                ],
                payload_hash: &empty,
            },
        );
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, \
             SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, \
             Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }

    /// S3 "Example: PUT Object": a path that needs encoding, and extra signed headers.
    #[test]
    fn put_object_matches_the_aws_example() {
        let payload = hex(&Sha256::digest(b"Welcome to Amazon S3."));
        assert_eq!(
            payload,
            "44ce7dd67c959e0d3524ffac1771dfbba87d2b6b4b4e99e42034a8b803f8b072"
        );
        let auth = authorization(
            &example_credentials(),
            "us-east-1",
            "s3",
            "20130524T000000Z",
            &Request {
                method: "PUT",
                path: "/test$file.text",
                query: &[],
                headers: &[
                    ("date", "Fri, 24 May 2013 00:00:00 GMT"),
                    ("host", "examplebucket.s3.amazonaws.com"),
                    ("x-amz-content-sha256", &payload),
                    ("x-amz-date", "20130524T000000Z"),
                    ("x-amz-storage-class", "REDUCED_REDUNDANCY"),
                ],
                payload_hash: &payload,
            },
        );
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, \
             SignedHeaders=date;host;x-amz-content-sha256;x-amz-date;x-amz-storage-class, \
             Signature=98ad721746da40c64f1a55b78f14c238d841ea1380cd77a1b5971af0ece108bd"
        );
    }

    /// S3 "Example: GET Bucket Lifecycle" has a query string.
    #[test]
    fn a_query_string_is_part_of_the_signature() {
        let empty = hex(&Sha256::digest(b""));
        let auth = authorization(
            &example_credentials(),
            "us-east-1",
            "s3",
            "20130524T000000Z",
            &Request {
                method: "GET",
                path: "/",
                query: &[("lifecycle", "")],
                headers: &[
                    ("host", "examplebucket.s3.amazonaws.com"),
                    ("x-amz-content-sha256", &empty),
                    ("x-amz-date", "20130524T000000Z"),
                ],
                payload_hash: &empty,
            },
        );
        assert!(auth.ends_with(
            "Signature=fea454ca298b7da1c68078a5d1bdbfbbe0d65c699e0f91ac7a200a0136783543"
        ));
    }

    #[test]
    fn paths_are_percent_encoded_but_keep_their_slashes() {
        assert_eq!(
            encode("/b/download/a_b-c.d~e", true),
            "/b/download/a_b-c.d~e"
        );
        assert_eq!(encode("/a b/$", true), "/a%20b/%24");
        assert_eq!(encode("a/b", false), "a%2Fb");
    }
}
