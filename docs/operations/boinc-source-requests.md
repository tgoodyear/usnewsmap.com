# Single-input source requests

The Temporal `ScienceSourceV1` entry point accepts one newspaper identifier
(`sn...`), exact retained batch/version (`..._ver01`) or page identity
(`sn..._YYYY-MM-DD_ed-N_seq-N`). It resolves a bounded, frozen set of scored,
non-English pages with actual indexed images, then the existing singleton
preparer prioritizes those members. Original ALTO extraction, hash/provenance
checks, real-pixel preparation, READY chunks, native audited publication and
independent-host quorum are unchanged. There is no competing submitter.

`qwen_source.py` runs on the producer host, using its existing private SSH trust
and preparation CT215. Registration is fsynced and keyed by the exact selector,
campaign and page cap; retries return the same hash-bound membership without
reselecting. Each request defaults to at most 10,000 pages; hitting that cap is
reported, not claimed as complete coverage of a larger newspaper. The manifest
and bounded progress summary live in `/var/lib/usnm-qwen-feed/requests`.

The normal source loop chooses remaining retained members of the oldest request
before ordinary replenishment. Already prepared/rejected/retained source
identities are excluded using the same existing disposition records. A changed
index checksum or archive version fails explicitly instead of silently changing
request membership. Requests waiting for unavailable archives do not starve
ordinary feeding. The 500-fresh-pair watermark, 2,000-unresolved ceiling,
three-READY-chunk bound, 50 GiB floor and reservation checks still apply.

`qwen_submit.py --source-status --sample -` is a read-only native report, not a
new submit path. It binds exact requested documents to submitted job hashes and
native audit receipts, and reads actual BOINC workunit/result state. Source
rejections and prepared pages are not counted as published workunits; replica
counts, publication and canonical consensus are separate. Pending audit,
preparation, publication, dispatch/admission, host results and science errors
remain visible. Workflow progress contains only selectors, hashes and aggregate
counts, not private file contents or credentials.

Deploy `qwen_submit.py` on the native server and `qwen_source.py` on the producer
before activating the Temporal source entry. Replace `qwen_feed.py` only at an
idle boundary; keep its existing service and receipts. Other callers can keep
using the ordinary inventory/preflight/submit interfaces.

This slice deliberately uses the **already indexed image corpus**. An identifier
without indexed eligible pages fails with an explicit acquisition requirement;
it does not fetch arbitrary source URLs or label metadata as available pixels.
A requested original archive not yet retained remains waiting for source
acquisition. Existing GPU cohort/finite admission and result-collection owners
continue automatically, but are not overridden: published jobs can legitimately
wait for admission/host requests. A native canonical result is agreement, not
proof of accuracy or downstream QUALITY publication. These remaining stages
must not be represented as complete merely because the source request was
registered or a BOINC job was published.
