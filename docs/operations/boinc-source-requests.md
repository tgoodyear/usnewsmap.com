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

If requested images become ineligible after registration, that tick tries later
requests and then ordinary selection without mutating frozen membership. An
all-rejected request needs science review; it is not native consensus. Rejection
markers are read from both single-page and bundled layouts. Purged BOINC history
is explicitly `native_history_unavailable` and needs review, while available
members keep reporting real progress; retained audit alone never implies agreement.

New handoffs retain a SHA256SUMS-covered original-index-to-JPEG provenance receipt.
The native audit copies it only when its JPEG bytes/hash match the audited job.
Source inspection checks that binding against the frozen batch/version and original
scan bytes/hash, not just the document ID. A changed binding or a legacy job without
one needs science review and cannot satisfy source consensus; deduplication remains
in force and no historical source receipt is invented or backfilled. Missing
bindings can instead be resolved by the explicit retrospective verification below.

Deploy `qwen_provenance.py` beside `qwen_submit.py` on the native server, and
`qwen_source.py` on the producer,
before activating the Temporal source entry. Replace `qwen_feed.py` only at an
idle boundary; keep its existing service and receipts. Other callers can keep
using the ordinary inventory/preflight/submit interfaces.
Deploy the matching `quality_feed.py` in preparation CT215 so new handoffs retain
their original-image binding before native audit. Legacy READY chunks still ship,
but their unverifiable original provenance remains explicit in source progress.

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

## Recovering missing historical image bindings

This is an operator-run evidence repair, not another feeder or permission to
accept unverified history. Keep the original `audit.json`, job IDs, submitted
markers, request membership and scientific results unchanged. Existing conflicting
bindings are never overridden. No work is resubmitted.

For each retained **single-page** native job, assemble a recovery target with:
`campaign`, `job_id`, `job_sha256`, `workunit_id`, the exact frozen `source`
fingerprint (`doc_id`, `batch`, `batch_version`, `bytes`, `sha256`), original
`scan` path, audited `jpeg` bytes/SHA256, and `transform`. Supported explicit
transforms are `reduced-gray`, `full-gray` and `full-rgb`; all encode JPEG at
quality 75. Reduced grayscale uses the existing JPEG2000 half-resolution rule.
These are reproducible representations, not an assertion of which software
invocation originally made the image.

Run the reviewed verifier in a bounded maintenance directory on the preparation
host, using its existing pinned Pillow/OpenJPEG environment and LAN scan server:

```sh
python qwen_provenance.py verify --input targets.json --output verified.json \
  --scan-server http://usnm-scanfetch.tjg.fyi:8380
```

Verification checks the retained original against the frozen bytes/SHA256,
reproduces the selected representation and requires **exact submitted JPEG
bytes/SHA256**. A mismatch fails explicitly. Output records a new verification
time, transform and decoder/verifier versions; it is not a recreated historical
handoff receipt. Preserve acquisition sidecars and archive evidence separately.

Transfer the reviewed proofs to a native maintenance directory, coordinate with
the existing publication owner and install under the existing bridge mutex:

```sh
python qwen_provenance.py install --input verified.json --share /project
```

Installation validates every target against retained native job bytes, actual
`page.jpg`, original audit and submitted marker before writing any receipts.
It creates atomic, no-overwrite files in
`qwen/<campaign>/recovered-provenance/<job_id>/<doc_id>.json`.
Source inspection consults them **only when the original binding is absent**,
rechecks the job/audit/source binding, and explicitly marks page provenance as
`verified_reconstruction`. These trusted operator receipts have the same
filesystem protection requirement as native audits; never install untrusted
or manually fabricated proofs.

Recovery can establish source identity, not BOINC agreement or OCR accuracy.
Missing native history/errors, independent-host quorum, admission and downstream
QUALITY acceptance retain their existing checks. If exact reproduction fails,
retain the gap for investigation or an explicit exclusion; do not infer a binding
from matching page IDs or visually similar images.
