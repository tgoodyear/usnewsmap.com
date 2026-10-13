# BOINC pilot extensions

The first BOINC pilot is #441. It runs `usnm-boinc` as the bridge and the worker, trusts its volunteers, and re-checks every result centrally. This page lists six things left out of that pilot, whether each fits its design, and what this change does about it.

The test for a fit: it must reuse the pilot's job and result contract (`usnm-ocr-contract::boinc`) and its provenance rules unchanged. Anything that forks them is a no-fit.

| #   | Item                       | Decision         | Why                                                                                                                     |
| --- | -------------------------- | ---------------- | ----------------------------------------------------------------------------------------------------------------------- |
| 1   | Fuzzy validator            | Fit, implemented | Reads the pilot's result files and manifest; adds nothing to the contract.                                              |
| 2   | Native app version         | No fit           | The contract names the container image digest in `engine.image`, and a result must carry the job's engine exactly.      |
| 3   | Custom work generator      | No fit           | The bridge already reserves under the lock and ledger and journals before `create_work`. A generator would bypass that. |
| 4   | Custom assimilator, naming | No fit           | `batch_collect_assimilator` and the `results/<batch>/<wu>__file_<name>` spool already do this job.                      |
| 5   | Fetch-and-OCR              | No fit for now   | See below.                                                                                                              |
| 6   | Scan-archive read path     | Future, doc only | The archive is not built yet. The job fields it would need are listed below.                                            |

## 1. Fuzzy validator (implemented)

Why it exists: the pilot's `sample_trivial_validator` accepts any result. That is fine for trusted friends, because the central collector checks each result against its job. For anonymous volunteers, a wrong but well-formed result would still reach the collector. Replicating each task and accepting only agreeing results lets the server drop bad ones earlier.

What it is:

- `crates/usnm-boinc/src/validate.rs` and the helper `usnm-boinc-validate`. BOINC's `script_validator` runs it with bare file paths and no flags: three paths check one result, six compare two.
- A result is valid when `result.json` parses as the pilot's `ResultManifest`, both digests match the files, the text is UTF-8 and the ALTO passes `usnm_alto::check`.
- Two results agree when they name the same job, scan and engine, and their text scores at least 0.98 on a word-multiset Dice measure (case and whitespace ignored). The threshold is a constant in code, not a setting. Tesseract is deterministic for one image, so honest replicas score 1.0; the slack covers float and thread differences between CPUs.
- It has no job file, so it cannot run `ResultManifest::matches`. The collector still does, so this is only an early filter.
- Off by default. `usnm-boinc submit --replicas N` (1 to 3, default 1) sets `min_quorum` and `target_nresults`. `ocr/boinc/daemons-fuzzy.xml` is the daemon line to use instead of the trivial validator.

Not done: adaptive replication, where trusted hosts skip replication. It is BOINC server configuration, and how it behaves here is UNVERIFIED.

## 2. Native app version (no fit)

A native app would run the same worker code without Docker. But a result's `engine` must equal the job's, and `engine.image` is the container digest. A native build has no such digest. Making it fit would mean either a second engine identity in the contract or a looser match, and both weaken provenance. The size of the pinned static Tesseract and model bytes would also be a new thing to verify.

If it is wanted later, it needs a contract change first: an `engine.kind` of container or native, with the native build's own pinned digest, and the collector accepting both.

## 3. Custom work generator (no fit)

The bridge reserves each page in its ledger under a lock, then journals, then calls stock `create_work`. A work generator that polls for pages would create work outside that journal and could submit a page twice. A generator would add nothing the bridge lacks. Possible small additions, not done: batches over 100 pages, and slicing a cohort across several `export` runs.

## 4. Custom assimilator and `.boinc.` naming (no fit)

The pilot's assimilator writes the spool that `collect` reads, and `collect` publishes through the same OCR-sync path as other engines. A second assimilator and a `.boinc.` file name would give the sync two ways to receive one result, so both are dropped.

## 5. Fetch-and-OCR (no fit for now)

Letting volunteers fetch the scan from the source themselves, instead of receiving it, spreads the load over many clients, each at a low rate. That is not circumvention of the source's limits, and the same load would be allowed from one place. Still, the pilot ships the scan with the task, which is simpler to check, and a better data source for the scans is preferable to a fetch from every volunteer. Revisit only if shipping scans becomes the bottleneck.

## 6. Scan-archive read path (future)

ADR-0018 (issue #442, PR #443) describes an archive of scans. If tasks should read from it, the job would need two fields beyond the pilot's `Scan`:

- `scan.archive_url`: where the scan can be read, under the same `https` and host rules as `scan.url`.
- `scan.ticket`: a signed, per-task, time-limited token that allows that one read.

The worker would try the archive with the ticket first and fall back to `scan.url`. The scan's `sha256` and `bytes` stay the check on whatever arrives, so the result contract does not change. This needs a contract version bump, and the archive's signing design is not settled. Not implemented.
