# Shadow science-pipeline inventory

Initial implementation for #1334, following the architecture in
[home-infra PR #90](https://github.com/tgoodyear/home-infra/pull/90).
This is a read-only reconciliation boundary, **not** a new feeder, authoritative
science ledger, Temporal deployment or permission to submit more work.

Run on an operator-owned, frozen copy of one native Qwen campaign's metadata:

```sh
python3 scripts/boinc/qwen_shadow.py --root /path/to/copied-campaign --max-units 10000
```

The copy contains `audit.json` and immediate `units/<source>/` directories with
`qwen.json` and the existing disposition/submission markers. No images, models,
credentials or network access are required. The helper does not create files;
stdout contains one complete versioned JSON observation only after reconciliation.
Failure is nonzero with a diagnostic on stderr and no success-shaped JSON.
Only metadata is read, with bounded file sizes and unit counts.

The output includes each job's exact manifest hash, declared source hash,
document ID, audited workunit ID and expected original replica count. A stable
`observation_sha256` covers canonical JSON of every field except that digest.
Repeating the same input produces the same observation, suitable as an
idempotency key for later shadow-ledger ingestion.

Counts are explicit: manifests/jobs, unique document IDs, audited workunits,
expected original replicas and explicit no-crops/oversized dispositions. A unique document
ID is not necessarily a unique useful scientific task: source revisions and
recipes can differ. Expected original replicas are not current BOINC ready,
assigned, returned or valid results.

An audit/manifest match means the copied receipt binds those exact metadata
bytes. It does **not** re-query BOINC, recompute Rust job identity, verify source
pixels/ALTO provenance, validate the full worker schema or establish scientific
acceptance. Every record explicitly sets `source_bytes_verified=false`.
Unaudited staged jobs remain `staged_unverified`; they are not eligible work
authorizations.

Uncertain `submitting` markers, submitted/audit mismatches, cross-job submission
marker names, null audit receipts, missing or changed
audited manifests, repeated job/workunit identities, unsupported bundles, symlink entries and duplicate JSON keys fail
explicitly. Do not repair markers or audits to make the observer pass. Reconcile
using the existing native publication procedure, preserving uncertainty/evidence.
The first slice deliberately supports only current single-page native jobs.
Campaign roots containing `pages/` are rejected even without bundle manifests,
so partial bundle-layout dispositions cannot be silently omitted.
No-crops and oversized markers are retained as hash-bound disposition observations,
not jobs; their underlying source/rejection evidence is explicitly unverified.
Bundle membership/page quorum remains separately owned and reviewed work.

The helper rechecks audit bytes, unit membership, manifests and marker hashes
before emitting output. That catches many accidental concurrent updates but is
not a filesystem transaction or a guarantee against transient/ABA changes.
Use a frozen metadata copy; do not treat a live-tree observation as admission
authority or hold the shared project lock for a historical scan.

## Validation and next boundary

```sh
python3 -m unittest -v scripts/boinc/test_qwen_shadow.py
```

Tests use synthetic data only and cover repeatability, non-mutation, count
semantics, uncertainty, missing/changed receipts, duplicate identities,
unsupported membership/quorum, bounds, symlinks and concurrent audit changes.
The existing script-unit discovery includes the new test file automatically.

Next: an isolated PostgreSQL ledger imports these observations idempotently,
retaining observation provenance and distinguishing observations from reservations.
A shadow Temporal workflow can then reconcile the same campaign without native
submission capability. Production adoption needs reviewed schema/roles,
capacity/restore tests, separate writer ownership and explicit rollout gates.
No NVMe or host shutdown is required for this metadata-only slice.
