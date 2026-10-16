# Host onboarding and OCR spot checks

`usnm-boinc host-trust` implements #527's controller commands, **OFF by default**.
Without `--execute`, commands only query the database and print a bounded plan;
they create no controller root or work. They install no timer, enable no
assignment, calibrate no threshold, change no trust, publish no OCR and modify
no production setting.

Coordinate scope and current runtime and worker releases in #529 before
execution. Resource, signing, client and activation holds still apply. Assignment
must already be enabled in project XML, and the released v2 worker's immutable
image must match the selected OCR engine. Commands cannot enable assignment.
Scientific calibration and real-host canonical-query acceptance remain required.

## Operator-owned configuration

Create a private JSON file with absolute paths (example values, not production
authorization):

```json
{
  "root": "/approved/controller",
  "project": "/approved/project",
  "jobs_root": "/approved/handoff",
  "mysql_defaults": "/approved/private/mysql-runtime.cnf",
  "mysql_ca": "/approved/mysql-ca.pem",
  "database": "usnmocr",
  "trust": "/approved/gold/trust.json",
  "prepared": "/approved/gold/prepared-campaign",
  "engine": {
    "base": "docker.io/example/tesseract@sha256:REPLACE_WITH_64_HEX",
    "dpi": 300,
    "image": "sha256:REPLACE_WITH_64_HEX",
    "psm": 3,
    "timeout_secs": 300,
    "version": "5.5.0"
  },
  "max_hosts": 10,
  "max_spots": 2,
  "sample_every": 100
}
```

Use the existing **private** MySQL option file, not a password argument. Group or
other permissions, symlinks and nonregular option files are refused. Queries use
TLS, hostname verification and the explicit CA. The account needs read-only
`host`, `workunit` and `result` access; stop for operator approval if absent, do
not grant access automatically. Connector errors are not echoed, because they
may contain sensitive diagnostics. No host, user or account database rows are
changed.

`prepared` contains `prepared.json`, `bundle.tar` **and the complete `cache/`
original-file set** from the existing `gold prepare` command. Each prepared remote
must be present under `cache/<remotes[*].name>` and match its size, MD5 and SHA-256.
Metadata and the bundle alone are not sufficient for mirrored gold submission.
`trust` is the existing calibrated v2 list: no default CER or
new approval is invented. `jobs_root` is the server-local root containing the
exported OCR job journals and staged inputs, not the controller root.

## Automatically discover and issue first gold

```sh
usnm-boinc host-trust onboard --config controller.json
# Only in the separately approved execution window:
usnm-boinc host-trust onboard --config controller.json --execute
```

Each pass reads BOINC's `host.id` inventory and skips hosts already trusted for
this exact immutable image. It issues one targeted gold unit per remaining host,
up to `max_hosts` (1–100). Repeated invocations discover later hosts without an
operator supplying host IDs. Inventory is bounded to 100,000 hosts; larger
projects fail closed instead of silently skipping hosts.

Campaign roots bind the prepared bundle, manifest, complete engine configuration
and host. Native gold write-ahead markers stabilize retries. One checksum-bound
bundle and checksum-verified cache serve each campaign, with regular-file hard
links into per-host roots and `cache/`; original bytes are not recopied per host.
Those roots must share a filesystem supporting hard links; the prepared source
may live elsewhere.
Native gold submission verifies and mirrors originals into BOINC downloads.

The shared validator expectations writer is locked and refuses updates over its
16 MiB reader limit **before** replacing the previous readable file. If that
bound is reached, stop for a reviewed sharding or retention decision; never
delete expectations for pending units to force another submission.

Submitted hosts are skipped without starving new hosts behind the per-pass cap.
A `submitting` marker blocks the pass and is **never automatically retried**.
Inspect that campaign's deterministic workunit on BOINC, then use the existing
gold reconciliation procedure: rename `submitting` to `submitted` if the
workunit is present, and remove it only after proving the workunit is absent. Do
not evade uncertainty by choosing another controller root. The controller's
kernel lock serializes its own dispatches.

Issuance does not promote trust or periodically recalibrate it. Review gold
reports and rebuild trust through the calibrated workflow; later failures revoke
trust. Reissuing failed hosts, starting a new campaign or scheduling the
controller requires an operator decision. No service is installed.

## Sample and rerun OCR independently

Provide a JSON array of existing original OCR job digests in `candidates.json`.
The array is bounded to 10,000 entries, deduplicated and sorted. Sampling uses the
first 64 bits of each digest modulo `sample_every` (positive integer); `1` samples
all candidates. Select this rate explicitly for the approved resource budget.

```sh
usnm-boinc host-trust spot-submit --config controller.json --jobs candidates.json
# Only in the separately approved execution window:
usnm-boinc host-trust spot-submit --config controller.json --jobs candidates.json --execute
```

For a sampled job, the controller reads the original canonical result's
`result.hostid`, requires same-image gold trust and selects a **different**
trusted host. A missing, zero or ambiguous host, another image, changed staged
inputs, or the lack of a second trusted host fail closed. At most `max_spots`
(1–100) new workunits are issued per pass.

The duplicate uses `usnm_spot_<job>_<source-host>_<target-host>`, the original job
request and verified scan bytes, and `create_work --target_host`. Verified inputs
are staged under **spot-specific physical filenames**, retaining the worker's
logical `page` and `request.json` names, so original-workunit file cleanup
cannot remove the second host's downloads. The duplicate's outputs cannot
replace the original `usnm_<job>` canonical spool. Each duplicate requests one
result with no automatic replacement; together with the original, the comparison
requires **two independent hosts**, not two results from one host. Stock
`sample_trivial_validator` is not itself evidence of agreement.

`spots/<job>/submission.json` is persisted as `submitting` **before** dispatch.
Any error or crash leaves uncertainty and blocks automatic retry. After checking
the exact workunit and target in BOINC, an operator may atomically set `stage`
to `submitted` if present, or remove that journal only after proving absence.
Existing submitted checks are skipped; changed canonical host or target identity
is refused rather than silently creating another check.

## Compare canonical results, without publishing or granting trust

After both results are assimilated:

```sh
usnm-boinc host-trust spot-compare --config controller.json --job JOB_DIGEST \
  --original-spool /approved/results/original \
  --spot-spool /approved/results/spot
```

The command re-queries both canonical hosts, checks that they equal the distinct
assigned hosts and are **still trusted for the same digest**, and validates each
result manifest, job identity, engine, scan, text and layout hashes and ALTO
structure using the existing contract. Terminal failure markers refuse
comparison. The two normalized texts must be exactly equal: no uncalibrated
fuzzy threshold is introduced. Agreement is a diagnostic quality quorum, not
identity proof or malicious-volunteer protection.

`spots/<job>/comparison.json` records job, image and host IDs, quorum, time,
agreement and reason, excluding page text and connector diagnostics. Attempts
with parseable config, root and job invalidate older comparisons before connector
preflight. Errors leave `status: unverified` and `agrees: false`; valid mismatches
record `text-disagreement` and exit nonzero. `status: verified` can describe
**disagreement**: check `agrees`. The file authorizes no future publication and
cannot override later trust revocation.

Preserve spools and journals after mismatches or failures; use quarantine and
review. No job stage, reservation, trust list, queue, canonical result or
published OCR changes. The trust-checked collector remains mandatory; this audit
adds no automatic publication gate or retroactive block. #527 stays open pending
approved controller scheduling, additional-host acceptance, gold calibration,
assignment canary and any publication-policy rollout.

## Show currently held results

```sh
usnm-boinc host-trust quarantine --root /approved/handoff
```

This read-only command lists existing collector quarantine records, including
host, image and job IDs, time and refusal reasons. It needs no database
connection or controller config. It omits old quarantine artifacts whose jobs
were subsequently published or marked failed, does not delete evidence, and
refuses malformed records or symlinks. The view is bounded to 10,000 directory
entries.
