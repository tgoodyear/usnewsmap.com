# Host onboarding and OCR spot checks

These **OFF-by-default** `usnm-boinc host-trust` commands implement the remaining
controller primitives in #527. Nothing installs a timer, enables assignment,
calibrates a threshold, changes trust, publishes OCR, or modifies a production
setting. A command without `--execute` only queries the database and prints its
bounded plan; it does not create a controller root or submit work.

Before any authorized execution, coordinate a current runtime and worker release
and the operational scope in #529. Existing resource, signing, client and human
activation holds still apply. The v2-capable worker must be released, its
immutable image must match the selected OCR engine, and active BOINC project XML
must already explicitly enable assignment. These commands refuse disabled
assignment; they never change it. Scientific calibration and additional
real-host canonical-query acceptance remain outstanding operational
requirements, not implied test passes.

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

Campaign roots are bound to the prepared bundle and manifest, the complete
engine configuration and the host. Native gold write-ahead markers make retries
stable. Each campaign stores one checksum-bound bundle; per-host regular-file
hard links share its storage instead of retaining a full bundle copy per
volunteer. Original cache files are checksum-verified and stored once in the
shared campaign, then hard-linked into every per-host `cache/`; no original
bytes are recopied per host. The prepared source may be on another filesystem;
controller per-host roots must share a filesystem supporting regular-file hard
links. Native gold submission then verifies and mirrors these originals into the
BOINC download tree before dispatch.

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

This is first-gold issuance, not automatic trust promotion or periodic
recalibration. Collect and review the gold reports and rebuild trust through the
existing calibrated workflow. A later failed report revokes trust as before.
Already-issued hosts are not silently reissued after a failure; any new campaign
requires an explicit operator decision. Repeated execution or scheduling of
this controller is a separate activation decision; no service is installed here.

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

`spots/<job>/comparison.json` records only the job, image and host IDs, the
quorum, the time and the agreement and its reason; it does not contain page
text or connector diagnostics. Every attempt whose config, root and job can be
parsed first invalidates an older comparison, before connector preflight.
Errors leave `status: unverified`
and `agrees: false`; valid mismatches record `text-disagreement` and exit nonzero.
Only validated comparisons have `status: verified`; this includes a verified
**disagreement**, so inspect `agrees` too. This file does not authorize future
publication or survive a later trust revocation as a fresh approval.

On a mismatch or failure, preserve the source spools and journal for
investigation and use the existing quarantine and review process. No native job
stage, reservation,
trust list, queue, canonical result or published OCR is changed. The ordinary
trust-checked collector remains mandatory. This independent audit does **not**
retroactively block an already published original or add a new automatic
publication gate. A scheduled controller, additional-host live acceptance, gold
calibration, assignment canary and any publication-policy change require their
separate approved rollout; #527 stays open until those operational criteria pass.

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
