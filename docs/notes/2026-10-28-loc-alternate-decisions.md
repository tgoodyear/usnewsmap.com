# Reversible third-party OCR decision previews

Follow-up to [explicit staging](2026-10-28-loc-alternate-staging.md), for
[#811](https://github.com/tgoodyear/usnewsmap.com/issues/811). This delivers an
operator-reviewed, append-only **preview**, not production selection. Normal
imports, release readers and OCR queues do not consult this history.

## Contract

`usnm-ingest ocr-alternate-decide --decision decision.json` appends one action.
`select` additionally requires `--comparison comparison.json`.
`ocr-alternate-preview --subject subject.json` reads the current preview.
Existing global state/store arguments precede the subcommand. Use an isolated
state and curated store for fixtures; no command fetches source artifacts.

The subject is an exact `doc_id`, `source_image_sha256`, `baseline_sha256` tuple.
A decision contains that `subject`, the exact `previous` decision hash (or null
for an empty history), an `action`, and nonempty bounded `reason` and `evidence`.
The action is one of:

```json
{"action":"select","candidate":"<staging candidate SHA256>","comparison_sha256":"<exact comparison bytes SHA256>"}
{"action":"retain-baseline"}
{"action":"reject","candidate":"<staging candidate SHA256>"}
{"action":"revoke","candidate":"<staging candidate SHA256>"}
```

Decisions serialize through the existing renewable OCR lock. Stale heads fail;
an exact retry of the latest decision returns its original receipt. Each record
hash binds the typed decision and its predecessor. History is limited to 256
contiguous records under:

```text
ocr-local/alternate-decisions/<doc_id>/<scan hash>/<baseline hash>/
  history/000000.json
  evidence/<comparison hash>.json
  commit/head.json
```

Comparison evidence and the create-only history record are written first.
The separately stored, atomically replaced `commit/head.json` binds the expected
history count and final hash. A missing/truncated history, including an empty
listing under a nonempty committed head, fails closed. An interrupted append
leaves one uncommitted record: preview refuses it; resubmitting that exact
decision finishes the head update. A different request cannot discard or adopt
it. Missing commit metadata alongside existing records also fails preview.
The final history slot must return to baseline, so the bound cannot strand an
active selection without a rollback slot.

Reject/revoke permanently block that candidate for this subject. If it was
selected, the preview returns to the baseline, not an earlier alternate.
`retain-baseline` also clears selection without deleting or rejecting anything.
Original staging artifacts, baseline bytes and earlier decisions are retained.
Negative actions validate the committed receipt identity, not healthy payload
bytes: damaged text/ALTO/rights cannot prevent rejection or revocation.

## Selection requirements and boundaries

Selection rereads the committed staging receipt and every artifact, including
registration and component-rights attestations. All three component rights must
be `cleared`. The checksummed comparison has this shape:

```json
{
  "subject": {
    "doc_id": "<canonical page ID>",
    "source_image_sha256": "<source image SHA256>",
    "baseline_sha256": "<baseline SHA256>"
  },
  "candidate": "<staging candidate SHA256>",
  "reference_sha256": "<human reference SHA256>",
  "reference_status": "human-verified",
  "reviewed_by": "<reviewer identity>",
  "compatibility_accepted": true,
  "legacy_ws_v1": {"reference_chars": 100, "baseline_edits": 20, "candidate_edits": 10},
  "raw_v1": {"reference_chars": 110, "baseline_edits": 25, "candidate_edits": 12}
}
```

The counts above are illustrative only. Both denominators must be positive;
neither candidate edit count may exceed its paired baseline count. This is a
conservative preview gate, not a change to production OCR admission.

**The comparison is a retained review attestation, not independently recomputed
CER or independently verified human/legal evidence.** The command does not
receive the reference text and cannot establish how the reviewer obtained it.
Real acceptance still requires authoritative scan/baseline registration,
independently cleared rights, genuine human reference, reproducible paired
scoring under the named rules, and reviewed release/queue integration. Merely
setting these fields does not satisfy those requirements or close #811.

Readers verify the committed head, complete history chain and derived selection. They reread
the selected candidate and its comparison, refusing missing/corrupt evidence
rather than returning a successful baseline fallback. Store metadata and
conditional bounded reads prevent oversized or concurrently changed artifacts
from being used. JSON inputs and comparisons are bounded to 64 KiB.
Exact decision retries also revalidate persisted selected evidence, not merely
the replacement comparison supplied with the retry.

## Evidence

Synthetic fixtures exercise A/B selection, stale-head rejection, exact replay,
revocation back to baseline, blocked reselection, baseline retention,
unknown-rights refusal, machine-reference refusal, regression and wrong-scan
refusal, corrupt selected comparison, corrupt retained text and history gaps.
The normal release listing remains empty.

The compiled ingest CLI additionally exercised staging, replay, unknown-rights
refusal, selection, preview, revocation and byte-conflict refusal against
temporary local stores with production credentials/network settings excluded.
These are implementation checks, not a real LoC quality or rights result.
