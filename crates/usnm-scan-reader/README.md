# Ticketed private scan reader (unwired)

`usnm-scan-reader` implements the separate axum service described in
[ADR-0018 decisions 6–8](../../docs/design/adr/0018-scan-archive.md) and #649.
**This is code and tests only.** It has no binary/listener, image, Azure resources,
role assignment, deployment workflow, search-API integration, or BOINC protocol
integration. The current offline volunteer pilot still stages scans centrally.

## Boundary and request contract

Construct `Service` with an `ArchiveReader`, configured Ed25519 **public** keys
indexed by key id, `Limits`, and a `Clock`, then obtain its standalone `router()`.
A future host must supply `ConnectInfo<SocketAddr>` from the actual connection.
`Forwarded` / `X-Forwarded-For` are not trusted. If a proxy is introduced, explicit
trusted-proxy identity handling needs review first; otherwise all proxied callers
share that proxy's IP budget. Do not mount this router in the search service.

The archive adapter is intentionally an injected abstraction, not an anonymous
HTTP reader. Its contract requires a service-owned managed/Entra reader identity,
a fixed private `scans` container, and `If-Match` on every ranged GET using the
properties ETag. A future production adapter must prove that contract against
Azure. It must never accept a caller URL, account key, SAS, or volunteer identity.
A changed object, authorization failure, or upstream error fails closed. No
source HTTP requests or automatic retries are performed by this crate.

Request:

- `GET /v1/scans/keep/<batch>/<lccn>/<date>/<edition>/<seq>.<jp2|jpg>`
- `Authorization: Bearer <base64url(canonical-json)>.<base64url(signature)>`
  (URL-safe base64 **without padding**; maximum token length 16 KiB).
- `X-USNM-Workunit: <exact signed workunit>`.
- Optional single `Range: bytes=start-end`, `bytes=start-`, or `bytes=-suffix`.
  Multiple/malformed/unsatisfiable ranges return 416 with `Content-Range`.
  A matching strong ETag in `If-Range` permits 206; a mismatch, weak validator,
  or date validator ignores the range and serves a separately budgeted full 200.
  HEAD is unsupported (405), and performs no archive I/O.

The signed JSON schema, in canonical field order, is:

```json
{
  "app_name": "ocr",
  "app_version": "1",
  "blobs": {
    "keep/batch/sn123/1900-01-01/ed-1/seq-1.jp2": {
      "bytes": 10,
      "md5": "<standard-base64 16-byte MD5>",
      "sha256": "<64 lowercase hex digits>"
    }
  },
  "expires_at": 2000,
  "issued_at": 900,
  "key_id": "rotation-id",
  "ticket_id": "unique-issuance-id",
  "workunit": "wu-1"
}
```

Sign the compact UTF-8 JSON returned by `Ticket::canonical_json()`: recursively
sorted object keys, no whitespace, integer Unix seconds only, exact fixed schema.
The sample above is illustrative, not a valid ticket. Unknown fields, duplicate
fields, noncanonical encodings, invalid signatures, unknown/revoked key ids,
future issuance, expired/overlong tickets, empty/oversized allowlists, invalid
blob names, sizes or checksums are rejected. No prefix permissions: each decoded
blob path and the workunit must match exactly. App name/version are signed
context, not a claim that a caller is authenticated as that app. A ticket is a
bearer capability shared by all holders until expiry; do not log its header.

## Accounting and response integrity

Default ticket bounds are three requests per signed page and three times the
sum of signed page bytes, 32 pages maximum, 64 MiB per scan, lifetime at most one
day. IP bounds default to 120 requests / 256 MiB per UTC minute; the replica's
UTC-day byte brake defaults to 20 GiB. These are adjustable code defaults, **not
approved production budgets**. Ticket/IP maps have separate finite capacities
(default 10,000 each); capacity exhaustion fails closed with 429. Expired entries
are reclaimed. Reservations are atomic across concurrent requests in one service.

Authorization precedes any archive I/O. Each authorized allowlisted request
reserves one request and its range/full bytes **before** properties/GET. Invalid
ranges reserve a request but zero bytes. If-Range expanding to full reserves the
additional bytes before GET, without charging another request. Failed property
lookups, 404s, disconnects, and failed body checks are not refunded. Repeated
small ranges cannot evade the request bound; failures cannot cause unbounded
Cold reads. Requests rejected at authorization/allowlist checks perform no
archive I/O and do not consume those authenticated-request reservations.

429 includes an integer `Retry-After` (ticket expiry, next IP minute/day window,
or 60 seconds for capacity). A miss is 404; upstream failure or mismatching
size/SHA256/MD5 metadata is 502. Error responses also use `private, no-store`.
Archive metadata/read/body operations share one bounded deadline (60 seconds
default, maximum configurable 1 hour), with no retry amplification.

Successful bodies are streamed, not whole-file buffered, with ETag, exact
Content-Length, Accept-Ranges, Content-Type, and Content-Range for 206. Full
representations include the stored `Content-MD5`; partial bodies deliberately do
not label a whole-file digest as the range's digest. Metadata must match the
signed job checksums/size. The adapter pins all reads to the metadata ETag.
Full bodies get incremental SHA256/MD5 validation; ranges get exact length
validation and require the client to verify job hashes after reassembly. The
final chunk is withheld until EOF/length/checksum validation succeeds. Earlier
chunks can already have been transmitted; a late failure aborts the body rather
than attempting to change a 200/206 already sent. Callers must reject incomplete
or hash-mismatching bodies. Reader adapters must yield bounded chunks, not
buffer entire blobs. The service holds at most the current adapter chunk.

Unlike the archive's stored public immutable Cache-Control, responses deliberately
use **`private, no-store`**: an intermediary must not cache a successful bearer
response and serve it after expiry or to a caller without the capability. There
are no public/anonymous URLs or credential redirects.

Clients retain the original `scan.url` as provenance, verify bytes against the
job, and on 404/429/5xx/timeout/checksum failure fall back to the original source
under its existing polite rate. This crate does not change that protocol or
contact a source itself.

## Explicit pre-deployment gates (still open)

1. **Durable budget semantics:** decide atomic cross-replica/restart admission,
   storage/backend, idempotency, failure/timeout and refund policy, day rollover,
   trusted time, key rotation/reissuance, abuse/capacity handling and approved
   numerical budget. Test concurrent replicas, process restarts, scale-to-zero
   and backend failure before claiming a durable cap. Current counters reset on
   restart, are not shared across replicas, and a reissued distinct signed
   ticket has a distinct allowance. A backwards clock cannot reset the daily
   counter, but this is not a durable replay/clock-rollback defense.
   **In-memory counters are NOT an account cost cap.**
2. **Signing-key vault placement:** decide the Key Vault, ownership, task-signer
   managed identity and least-privilege access, rotation/revocation overlap,
   compromise response and key-id distribution. Private signing material must
   remain in Key Vault/the authorized signer, never in this service, a volunteer
   input, configuration checked into Git, or the repository. No production signer
   or private key is provided here; the tests use an explicitly synthetic seed.
3. **Activation review:** separately approve any network-enabled volunteer
   protocol, production identity adapter/conditional-read integration tests,
   trusted ingress IP handling, TLS, admission/concurrency/header/body-size
   controls, monitoring without ticket logging, and resource/replica/cost policy.
   Provisioning, exposing an endpoint, BOINC changes and deployment need separate
   authorization. Until then the existing identity-based bridge path is unchanged
   and #649 remains open.

Local validation: `cargo test -p usnm-scan-reader` and
`cargo clippy -p usnm-scan-reader --all-targets -- -D warnings`.
