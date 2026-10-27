# The gold check

Part of the [BOINC batch pilot guide](README.md).

## Gold check

For an optional, aggregate-capped client beside an OCR node's existing queue
worker, see [the dev-first OCR-node plan](../../docs/boinc-ocr-node.md) (#472). It does not
enable or attach production clients.

A volunteer host can score its engine on the public gold bundle
([ocr-gold-set.md](../../docs/ocr-gold-set.md#volunteer-bundle), #457) and report CER and
WER together with the bundle version. Two steps, because the task container
runs with `--network=none`:

```sh
# On the host, with network: verify the bundle, download the original files it
# names (BLN600's zip from figshare, page images from loc.gov) into a cache, and
# keep each only if its sha256 matches. Downloads are one at a time, 1 s apart.
usnm-boinc gold-stage --bundle usnm-ocr-gold-v1.tar --cache CACHE --work WORK

# Offline: verify the bundle again, check the cache, cut the crops locally, run
# Tesseract on each, score as gold.py does, and write the report.
usnm-boinc gold-run --bundle usnm-ocr-gold-v1.tar --cache CACHE --work WORK \
  --out gold-result.json [--tesseract tesseract] [--psm 3] [--dpi 300] [--limit N]
```

For another engine, replace `--tesseract` with `--engine-cmd /path/to/adapter
--engine-name NAME --engine-version VERSION`. The executable receives exactly
two arguments: the crop image path and its bundle language. It must write only
UTF-8 OCR text to stdout and exit zero. Use an adapter executable for extra
flags; no shell expansion is performed. Each crop has the same timeout; output
is capped at 16 MiB. External commands require Unix; their process group is
killed on completion, failure, timeout and cancellation, including ordinary
adapter subprocesses. Detached sessions are outside this containment (use a
container for untrusted engines). Failed command output (including stderr) is
never copied into the report. The profile is the bundle language; psm and dpi
describe Tesseract only and do not configure an external engine. Name and
version are explicit operator metadata, not independently verified image
identity.

`gold-run` does not download files, but it cannot sandbox an arbitrary
executable. Run the **entire invocation** in a container with `--network=none`
(as for the stock worker), with preinstalled engine and models and a staged
cache; never give the adapter reference text. This option is for local
experiments, not the locked `usnm_gold` BOINC job or its trust reports.

`gold-result.json` holds `bundle_version`, the manifest's sha256, the engine
(name, version, psm, dpi), one row per crop (counts only: reference characters,
character edits, reference words, word edits, the model profile and the time), a
summary per source and overall, the `--limit` if one was given (a limited run is
partial), and the crops that were skipped with the reason (not staged, checksum
mismatch, no installed model, engine error, over the limit). It holds no page
text and no transcription. The project does not ship derived BLN600 crops or LoC
crops: it mirrors the unchanged original files from the publisher and LoC fetch
cache, and the client cuts the crops locally.

### As BOINC work: `usnm_gold`

The third app, `usnm_gold` (#462), sends the same check to hosts as work. It
runs in the same worker image; `gold.json` in the slot selects it. The job and
its checks are `usnm_boinc::gold::job`; the code is `crates/usnm-boinc/src/gold/`.

**Inputs and delivery.** A unit's inputs, in input template order:

| Open name              | From                                                               | On the host  |
| ---------------------- | ------------------------------------------------------------------ | ------------ |
| `gold.json`            | the project server                                                 | per unit     |
| `bundle.tar`           | the project server (shareable files only)                          | sticky, kept |
| `<sha256>.zip`, `.jpg` | the project server's mirror of figshare and tile.loc.gov originals | sticky, kept |

`gold prepare` downloads the originals into the campaign cache and records
their sizes, MD5s and SHA-256s in `prepared.json`. Before creating work,
`gold submit` verifies each cached file against those values and mirrors it
once into the BOINC project's ordinary download hierarchy under a
content-addressed physical name. `create_work` receives those staged filenames,
not `--remote_file` URLs; the volunteer client transfers the project copy,
not the publisher copy. A pre-existing mirror file must match the prepared
manifest or submission stops without replacing it.

The [Figshare v2 record](https://doi.org/10.15131/shef.data.25439023.v2)
identifies CC BY-NC-ND 4.0 and credits Callum Booth, Alan Thomas and Robert
Gaizauskas. The [CC BY-NC-ND 4.0
deed](https://creativecommons.org/licenses/by-nc-nd/4.0/deed.en) and [legal
code](https://creativecommons.org/licenses/by-nc-nd/4.0/legalcode.en) describe
sharing subject to NonCommercial use, appropriate credit and a license link, and
do not permit sharing adaptations. The staged zip is copied unchanged. The
bundle's `licenses/bln600/` directory preserves the source `LICENSE` and
`readme.md`, plus `SOURCE.json` and `ATTRIBUTION.txt`; the bundle is delivered
as a separate sticky input alongside the zip. These files record the published
conditions; the locked v1 `SOURCE.json` is retained as historical source
metadata, not a legal determination or confirmation that a particular deployment
qualifies as NonCommercial. Before enabling a mirror, the operator must verify
the project and hosting use against the source terms. Do not distribute
BLN600-derived crops or re-encoded images. The LoC images are likewise
transferred from the verified mirror.

The tar and originals are marked `<sticky/>` and `<no_delete/>` in the input
template, intended for reuse across a host's gold units. Live client evidence
has not confirmed that the generated work retains these flags or avoids repeat
downloads (see the acceptance record below). All are `<copy_file/>`, because
the container sees only the slot. Their open names match the cache names
`gold-run` reads, so the slot is the cache and the run step needs no network:
the container keeps `--network=none` from `job.toml`. The worker checks every
input's size and sha256 against the job, the manifest's digest and version, the
Tesseract version and base image, and the model bytes of each profile it used,
then writes `gold-result.json`.

**Result.** `gold-result.json` is the report above, run on the whole bundle
(no `--limit`), with a `boinc` block: job id, worker image and base digests,
and the host id the client wrote into the slot's `init_data.xml` (the client's
claim, not proof). Counts only; no text. The output template allows 256 KiB.

**Validation.** One result per unit (`min_quorum` 1): each result measures the
host that ran it, so results from two hosts are not expected to match, and
there is no compare script. `script_validator` runs `usnm_gold_check`
(`usnm-boinc gold check`) on each result against
`<project>/gold/<manifest sha256>.json`, which `gold submit` writes. It
refuses a result with any field outside the schema, a skip reason other than
`no-model:<lang>`, `engine-error` or `not-prepared` (the worker turns its
reasons into these codes), or an engine name other than `usnm-tesseract`, and
refuses it unless:

- it is the report of the unit it was uploaded for (the job id in it matches the
  workunit name in the result file's name), a unit this server made, with that unit's bundle version, manifest
  sha256 and engine (image, base, version, PSM 3, DPI 300), and no `--limit`;
- every crop of the bundle is scored or skipped exactly once;
- each scored crop's reference characters and words equal the counts `gold
prepare` computed from the transcription (a host that read other text fails);
- edit counts and times are in range, and the summary adds up from the crops.

This does not prove which engine ran. As for the other apps, volunteers are
trusted friends.

**Where reports go.** `usnm-boinc gold collect` reads canonical results from
the `batch_collect_assimilator` spool and validates them again. The bridge also
queries the read-only BOINC database for the campaign's submitted workunits and
writes `native-outcomes.jsonl` beside `reports.jsonl` and `crops.jsonl` under
`<share>/gold/<campaign>/out/`. The native table is capped at 10,000 results and
16 MiB; it contains only app/workunit/result/version IDs, host ID, immutable job
image and challenge identity, explicit calibration scope, BOINC state/outcome/exit
status, and server send/receive epoch timestamps. It contains no OCR text, stderr or
filesystem times. Canonical report rows bind their result ID, host and ordering
timestamp to the matching native server row. An absent/ambiguous DB result or
host mismatch fails closed for calibrated trust; no zero-crop report is
synthesized. The files remain on the existing project share. Recollection merges the bounded
previous native snapshot before refreshing it, preserving captured terminal
history after BOINC purges result rows. A terminal assimilator marker with any
unresolved native result (including a stale in-progress row) makes collection fail
before replacing reports or outcome evidence.

**Models.** The worker image's model lock decides which crops a host can score;
crops in a language with no locked model are listed as skipped.

**Running a campaign.** With the server running the bridge image, which carries the validator (the
on-prem server does; an Azure server needs `"assessment": true` in
`boinc/infra/boinc/env/<env>.json`, then `boinc/scripts/provision-boinc.sh`), a
`usnm_gold` app version installed (`boinc/scripts/install.py --app usnm_gold
...`), signed, and `bin/update_versions` run, and the bundle tar copied to
`<share>/gold/<campaign>/bundle.tar`:

```sh
# Once per bundle: fetch the originals, one request at a time, 5 s apart, into
# <campaign>/cache on the share; write prepared.json (sizes, md5s, reference
# counts). Stops if any download fails or a checksum differs.
boinc/scripts/onprem-step.sh --env prod --target bridge --step gold-prepare --var GOLD_CAMPAIGN=v1-tess550 --wait-mins 60
# Make the units (default 20). Re-running skips units already submitted.
boinc/scripts/onprem-step.sh --env prod --target bridge --step gold-submit --var GOLD_CAMPAIGN=v1-tess550 \
  --var GOLD_ENGINE_IMAGE=sha256:... --var GOLD_ENGINE_BASE=<its TESSERACT_IMAGE> --var GOLD_UNITS=20 --wait-mins 30
# When results are assimilated, use the calibration's explicit scope. This
# reads the BOINC result table with the bridge's read-only DB account:
boinc/scripts/onprem-step.sh --env prod --target bridge --step gold-collect --var GOLD_CAMPAIGN=v1-tess550 \
  --var GOLD_SPOOL=/project/usnmocr/results/0 \
  --var GOLD_LANGUAGE=<source-language> --var GOLD_PROFILE=<model-profile> --wait-mins 30
# Then the trust list for `collect --trust` (see Host trust below), at <share>/gold/trust.json:
boinc/scripts/onprem-step.sh --env prod --target bridge --step gold-trust --var GOLD_CAMPAIGN=v1-tess550 \
  --var GOLD_MAX_CER=<calibrated-threshold> \
  --var GOLD_MANIFEST_SHA256=<calibrated-manifest-sha256> \
  --var GOLD_LANGUAGE=<source-language> --var GOLD_PROFILE=<model-profile> --wait-mins 10
```

These steps run on the on-prem host, and `boinc/scripts/onprem-step.sh` requires
`--wait-mins`, so each command carries one. A `"server": "azure"` environment starts the
same steps with `boinc/scripts/start-boinc-bridge.sh`, where `--wait-mins` is optional.

The gold units' batch is 0, so their spool is `/project/usnmocr/results/0`.
These commands describe the code path; no production mirror or campaign is
enabled by this change.

For separately authorized LoC-only transfer acceptance, `gold-mirror-loc`
stages only the 18 original JPEGs pinned in `bundle-v1.lock.json`, without
opening BLN600, fetching upstream data, submitting work, or changing clients:

```sh
boinc/scripts/onprem-step.sh --env prod --target bridge --step gold-mirror-loc --var GOLD_CAMPAIGN=v1-tess550 --wait-mins 30
```

Use the normal serialized production-job preflight. The command first checks
all selected cache files against prepared size, MD5 and SHA-256, and the locked
URL/SHA-256, and checks existing destinations. It rejects unexpected paths,
symlinks and mismatches, then publishes mode-0664 files atomically using native
`dir_hier_path`. JSON receipts list physical paths and checksums. A partial
publication after an I/O failure is safe to retry; existing matching files are
not replaced. This is not BLN licence clearance or sticky-client acceptance.
Verify anonymous HTTP GET bytes and headers separately; proving client reuse
still requires two units on the same compatible attached host.
In a fresh bridge job, the wrapper writes only `download_dir` and the committed
template's fanout to the container-local config target. It refuses an existing
local config or unexpected shared config symlink, never replaces that symlink,
and never fetches credentials. The server replica's own config is unaffected.

A unit left with a `submitting` marker is handled as for the assessment.

### Host trust for `usnm_ocr`

`usnm_ocr` validates with `sample_trivial_validator`, which accepts any
well-formed result. So `collect` can gate publication on the gold check (#468):
a result counts only when the host that returned it has a gold report for the
same engine image (the worker image digest) on the exact calibrated challenge,
and the page's source language and model profile match the declared calibration,
with every crop scored, no skips, no crop with character edits at least its
reference length, and overall CER at or below a threshold. The per-crop guard
rejects complete loss (including empty output) even when an aggregate hides it;
it is not a guarantee against subtler OCR errors. There is **no default
scientific threshold**: calibrate on the first campaign for the locked image,
bundle, languages and scoring configuration;
record that evidence and pass `--max-cer` (bridge: `GOLD_MAX_CER`) and
`--manifest-sha256` (bridge: `GOLD_MANIFEST_SHA256`), `--language`
(`GOLD_LANGUAGE`) and `--profile` (`GOLD_PROFILE`) explicitly.
Gold currently supports PSM 3 / 300 DPI only; the trust artifact records these,
and different page configurations are refused. The collector records observed
profiles and scoring configuration, and every scored crop in an eligible report must use the declared profile.
A newer valid canonical report with a different/mixed profile or failed crop
quality revokes earlier eligibility.
The operator must establish the declared
source-language scope from the pinned challenge and calibration evidence;
observing an English model profile alone does not establish an English source.
One artifact covers one language/profile/configuration, not every language
supported by its image.

An earlier MVP's 0.10 was not calibrated. Trust format v5 rejects old v1-v4
lists for publication. Gold onboarding treats a missing or legacy list as no
trusted hosts so it can issue new challenges. Recollect each campaign with the
read-only native result query and explicit `GOLD_LANGUAGE` / `GOLD_PROFILE`, then
rebuild from its validated `reports.jsonl` and `native-outcomes.jsonl`; old
reports without native result IDs and server timestamps are insufficient for
v5. Native collection records outcomes only from workunits whose server-side
assimilation is done (`assimilate_state=2`), with the database's total result-row
count; it rejects a snapshot unless every result row is present and over. Trust
rebuild repeats this completeness check on the saved metadata. A previously saved
pre-final snapshot is not upgraded from a later partial view, because a purged
retry cannot otherwise be ruled out. Do not edit or relabel legacy lists.
Deploying this guard neither chooses a threshold nor promotes hosts.

A terminal attempt revokes the matching host's **quality eligibility** only
when the authoritative BOINC row is over (`server_state=5`), reports
`CLIENT_ERROR` (`outcome=3`), is not a cancellation (`exit_status` is neither
202 `EXIT_ABORTED_BY_PROJECT` nor 203 `EXIT_ABORTED_VIA_GUI`), identifies a
nonzero host, and has a server `received_time`. This narrowly treats a returned client execution error (such
as the all-skipped Gold worker exiting without a report) as a conservative
eligibility failure; it is not a measured CER/WER result and must not be
reported as OCR quality. Project and user cancellations (`exit_status=202` or `203`,
`EXIT_ABORTED_BY_PROJECT` or `EXIT_ABORTED_VIA_GUI`) are excluded even though
BOINC may record them as `CLIENT_ERROR`. Other outcomes—including server-side
inability to send, no reply, validator/server errors, missing output artifacts,
or rows without an attributable host/receive timestamp—do not revoke trust. They
remain separately visible as native metadata or collection failures and require
operator review if needed. No inference comes from spool or filesystem times,
and no canonical report is fabricated for zero scored crops.

The native app/result/host identity and server timestamps come from the BOINC
database; the immutable worker-image digest and challenge/configuration are
bound by the digest-derived workunit name and validated submitted job. Language
and profile are explicit calibration scope, never guessed from a missing
report. Eligibility events are ordered by server receive time per image, host,
challenge, language, profile, PSM and DPI; the newest event wins and any tie
fails closed. A negative event in one declared scope cannot revoke another
image or language/profile/configuration scope. The host ID remains a BOINC
quality signal, not proof of volunteer identity or protection against malicious
hosts.
The handoff spool shares one global `gold/trust.json`; passing a separate local
file does not isolate its policy to one job. Both native and handoff publication,
and independent-host spot replay/comparison, enforce the job scope. English-only
evidence never implicitly authorizes German, French, Japanese, another model
profile or another page segmentation/DPI configuration. When additional
`(language, profile)` scopes are calibrated and promoted into the shared
`trust.json`, `usnm-boinc gold trust` merges the newly promoted scope into
`policies` without overwriting or invalidating existing scopes (such as
`eng/eng`), while remaining backward-compatible with existing calibrated v4
single-scope `trust.json` files. Collection and spot-check planning look up the
job's exact `(message.language, engine.profile)` entry and check its `psm`,
`dpi`, `manifest_sha256`, `max_cer` and per-image `hosts` map, so a host trusted
only for `eng/eng` is refused for `deu/frak2021` work (and vice versa).

```sh
# Build or merge a calibrated scope from canonical reports plus native server
# outcome snapshots (preserving any other scopes already in --out or --merge).
usnm-boinc gold trust --reports <share>/gold/v1-tess550/out/reports.jsonl \
  --outcomes <share>/gold/v1-tess550/out/native-outcomes.jsonl \
  [--reports ...] [--outcomes ...] --max-cer CALIBRATED_THRESHOLD \
  --manifest-sha256 CALIBRATED_MANIFEST_SHA256 \
  --language CALIBRATED_LANGUAGE --profile CALIBRATED_PROFILE \
  [--merge <share>/gold/trust.json] --out <share>/gold/trust.json
# The canonical result's host, from the project database:
#   select r.hostid from workunit w join result r on r.id = w.canonical_resultid
#   where w.name = 'usnm_JOB_ID';
usnm-boinc collect --root ROOT --spool SPOOL --job JOB_ID \
  --curated-url CURATED_STORE --out-url OCR_OUT_STORE \
  --trust <share>/gold/trust.json --host-id HOST_ID
```

With `--trust`, a result from an untrusted host, or with no `--host-id`, is held
back: `collect` copies its spool files and a `reason.json` to
`<root>/quarantine/<job>/`, publishes nothing, leaves the job's stage as it was
and exits non-zero. Once the host passes a gold unit, rebuild `trust.json` and
run `collect` again. Without `--trust`, `collect` publishes as before;
production collection must use the existing hand-off `collect` step: it always
requires trust and a canonical host for successful work, refusing a missing list
or host. `spool-out` already queries
`workunit.canonical_resultid -> result.hostid`; the hand-off rejects an operator
override conflicting with that host. Only terminal failure markers bypass trust
to mark failure, never to publish. The host ID written by the Gold worker is a
client claim; v5 native collection cross-checks it against the authoritative
BOINC canonical result. This remains a quality signal for trusted volunteers,
not an identity check.

Trust is keyed by the **full immutable worker digest**, not a tag, base image or
engine version. In v5, server `received_time` orders eligible canonical reports
and attributable terminal client errors within the calibrated scope, regardless
of file order. The newest event wins; same-time conflicts fail closed. Missing
CER never grants trust. Client `completed_at` is retained report metadata, not
the native eligibility clock. The historical v4 pilot used client-clock ordering;
its receipts do not establish deployment or live acceptance of v5.

`gold submit --target-host HOST` (bridge: `GOLD_TARGET_HOST`) provides a
targeted, write-ahead campaign; its persisted campaign nonce and host
participate in the validated job identity, so fresh roots and hosts cannot
collide in workunit or spool names and retries keep the same identity. Targeted
jobs use protocol v2 and require a **newly built and released worker containing
this support**; do not target old worker images that only understand v1.
Untargeted v1 job bytes and identities remain compatible. Use a fresh campaign
root per host and image. It refuses until the project's active XML config
explicitly has `<enable_assignment>1</enable_assignment>`. This PR does **not**
enable assignment on production: first verify scheduler and assignment behavior
in dev and calibrate the first campaign. Live acceptance of discovery and
issuance for new hosts, canonical host-query acceptance, and periodic spot-check
activation remain rollout blockers; no automatic publication or scheduler
configuration has been deployed. The OFF-by-default [host-trust follow-up
controller](../../docs/notes/2026-10-16-boinc-host-trust-followups.md) now provides bounded automatic host
discovery and first-gold issuance commands, and sampled independent-host OCR
rerun and comparison commands. Dispatch requires an explicit `--execute`; no
timer, trust promotion or assignment enablement is installed.

### Build the gold bundle

The bundle tar can be built on the BOINC host, so nothing passes through an
operator's machine. The step needs a bridge image that has it, so build it first
(`boinc/scripts/build-boinc-images.sh`, which rewrites `images.bridge`) and set `SERVER_IMAGE`
in `/etc/usnm-boinc/server.env` to it, which the step runner reads. Then run the bridge
image's `gold-bundle` step as root on the host:

```sh
usnm-boinc-step --env prod --step gold-bundle --var GOLD_CAMPAIGN=v1-tess550 --wait-mins 360
```

`"goldBundle": true` in `boinc/infra/boinc/env/<env>.json` applies to the Azure
server only. With `"server": "azure"`, the bundle is built inside the VNet instead: set it
in the same commit as the new digest (it is false until then). After
`boinc/scripts/provision-boinc.sh`, the stack has the gold bundle job (`usnm-boinc-gold-caj-<env>`, the stack's
output `goldBundleJobName`). It runs the same step with `GOLD_CAMPAIGN=v1-tess550` set in
the job itself, so it needs no override:

```sh
az containerapp job start --subscription <subscription> --resource-group usnm-boinc-rg-prod \
  --name usnm-boinc-gold-caj-prod --query name -o tsv
```

The step (`boinc/images/bridge/gold-bundle.sh`):

1. runs `gold.py public-fetch` for `bln600`, `overproof-ca` (`--pages 20`),
   `newseye-bnf`, `newseye-nlf-fi` and `newseye-nlf-sv` (`--subset validation`)
   and `nzz` into `<share>/gold/build-v1/data`. gold.py keeps its own pacing:
   one request at a time, a second apart, and loc.gov paced as `locfetch` paces
   it. A file that's already downloaded isn't fetched again, so a rerun resumes;
2. runs `gold.py public-convert` for each, with the flags of the 13 October 2026
   runs ([ocr-scoring-guide.md](../../docs/ocr-scoring-guide.md#fetch-and-convert)): `--count 600 --per-page 1`
   for BLN600, `--count 600 --per-page 50` for Overproof, `--count 60` for the others;
3. runs `usnm-ocr-survey bundle-build --version 1 --seed 457 --per-source 30` with
   BLN600's zip as `--original`, writing its lock beside the bundle, then
   `bundle-verify` on the tar;
4. stops, and installs nothing, unless that lock equals the committed
   `fixtures/ocr-gold/bundle-v1.lock.json`, the tar's sha256 is
   `42520de42c7597bef4ea0ef3508797bfb7db129dd09c31e3d5a2236222be9183` and the
   manifest's is `150e20d75a5357caeeb6eb63911a3d94348e617879f7b0de16c7b1a54670fe3a`;
5. writes `<share>/gold/v1-tess550/bundle.tar` (beside it first, then renamed).
   A bundle already there with that sha256 ends the run at once; one with another
   sha256 is refused. One run at a time: a second run stops on the lock
   `<share>/gold/.build-v1.lock`.

New bridge builds carry Pillow 13.0.0 (the CI's version, from its hash-pinned
manylinux wheel) and bsdtar for BLN600's AES zip. The v1 recipe previously
used Pillow 11.3.0. The crops are JPEG re-encodes, so a different Pillow or
libjpeg could change the tar's bytes, and step 4 must still catch that.
The historical v1 recipe no longer reproduces with current Overproof
geometry; see the versioned rebuild below. The job allows six hours
(`--wait-mins 360` on the host).

**Pillow 13 and bundle v2 (26 October 2026):** a full, network-disabled
conversion of the original six cached datasets produced 142 bundle crops.
The regenerated v1 lock differs from the historical lock because #660
corrected Overproof ALTO geometry: 17 selected Overproof crop IDs change.
A control conversion under Pillow 11.3.0 produces the same Overproof
manifest as Pillow 13.0.0, excluding the encoder label. The other five
datasets' converted manifests likewise differ only in that label (BLN600's
unchanged original crops have no encoder change). This is pre-existing
recipe drift, not evidence of a Pillow JPEG regression.

The corrected recipe is recorded separately in
`fixtures/ocr-gold/bundle-v2.lock.json`: tar SHA256
`1e0fcc7bdc6bb6840658a5f615ad5f6f5a5853598cb2a47a45139c800c67446d`,
manifest SHA256
`f7fe39f70b61ba13ce5de1ec85f1cd8b93c07ef9b01518e02bb5abb38b3cffd3`.
No v1 lock, bundle, submitted job or trust record is replaced.

New builds must explicitly choose v2 and a fresh campaign:

```sh
usnm-boinc-step --env prod --step gold-bundle \
  --var GOLD_CAMPAIGN=v2-tess550 --var GOLD_BUNDLE_VERSION=2 --wait-mins 360
```

The default remains v1 for existing callers. A fresh v1 conversion with the
current geometry intentionally fails its historical lock; an existing
verified v1 artifact is still accepted. Never weaken that check.
`gold-bundle.sh --version 2 --source-cache /sources --share /isolated-share
--campaign v2-check` supports an offline reproduction, with the original
dataset cache mounted read-only and no installed bundle in the destination.
The script selects the committed version's lock and expected hashes and
keeps version-specific build directories. All versions share the legacy
build lock to prevent overlapping installs.

Building or installing v2 does not submit work, promote hosts or recalibrate
trust. Calibrate a new campaign explicitly before using v2 as a trust
challenge. The `gold mirror-loc` convenience command remains deliberately
restricted to the locked v1 manifest; v2 uses `gold prepare` and the normal
verified original-file staging during submission.

Then run `gold-prepare` as above.

### Live acceptance record and remaining checks

The production `v1-tess550` campaign completed on 14 October 2026. `gold-collect`
read the batch-0 assimilator spool and wrote six report rows and 540 crop rows.
Hosts 2 and 3 returned four and two valid results respectively; each scored 90
crops and skipped 52 for unavailable language models. Both hosts reported CER
0.0905 and WER 0.2366. The successful reports establish an end-to-end path through
the stock client, worker and server validator, but do not prove every transport
or runtime detail below.

| Acceptance check                                                                             | Evidence and status                                                                                                                                                                                                                                                                                            |
| -------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Legacy campaign: publisher fetch, BLN600 MD5 and bridge access to figshare / tile.loc.gov    | **Passed end to end on 14 October:** the campaign prepared publisher inputs and valid tasks scored BLN600 and LoC-backed samples. This predates project mirroring; it is not mirror acceptance evidence.                                                                                                       |
| Wrapper slot contains `init_data.xml`                                                        | **Passed end to end:** accepted reports carried host IDs 2 and 3.                                                                                                                                                                                                                                              |
| Validator finds the expected bundle data                                                     | **Passed in production; mechanism unverified:** `script_validator` accepted all six reports. The run did not establish whether `PROJECT_DIR` and `PROJECT` were present or the fallback/`USNM_GOLD_EXPECTED` path was used.                                                                                    |
| `<sticky/>` / `<no_delete/>` survive `create_work --remote_file`; a second unit reuses files | **Partly verified:** production WUs 1 and 4 retain both flags in their database XML (16 October 2026). No transfer log or before/after cache record established that later units avoided downloads.                                                                                                            |
| Current ordinary-file mirror path                                                            | **Not deployed or live-tested:** `gold submit` verifies originals against `prepared.json` and stages them as ordinary project downloads; repeat-unit cache behavior still needs client transfer evidence.                                                                                                      |
| Real-host duration and resource use                                                          | **Partly measured:** two Gold results in the related live acceptance recorded 163.618 s elapsed each; the host's cgroup limits were 1 CPU, 1.5 GiB memory, no swap and 64 PIDs. Peak memory and disk consumption were not measured, so `rsc_fpops_est=5e12` remains a guess.                                   |
| Volunteer guide on a clean Debian/Ubuntu host                                                | **Debian host readiness passed:** clean Debian 13 x86-64 full VM, official BOINC 8.2.13, Podman 5.4.2 and AppArmor enabled; setup and restart passed mandatory slot, isolation, kernel-limit and pause/resume probes (16 October 2026). No project attached; new task acceptance and Ubuntu remain unverified. |

For a repeatable acceptance record, retain per-host and per-workunit evidence
for these same checks: result upload and validation; the result's non-null host
ID matched to the server's canonical host; client transfer and cache evidence
across two units on one host; mirrored-file hashes against `prepared.json`;
validator success and how it resolved its expected-data directory;
`gold-prepare` completion; task elapsed time, peak memory and disk; and the
clean-host guide's command output. Do not infer cache reuse from multiple
successful tasks or infer peak resource use from cgroup limits. Keep the
production issuance and campaign separate from this documentation checklist.

Volunteers' steps are in [boinc-volunteer.md](../../docs/boinc-volunteer.md).

### Engine routing

Production selection is shared through `usnm_ocr_contract::routing::EngineKind`
and `EngineRoute { kind, profile }`. `EngineKind::for_language` chooses
`NdlocrLite` (`ndlocr-lite`) for `jpn` and `Tesseract` for every other language.
`usnm_ocr_worker::engine::candidates` exposes engine and profile preferences
(Japanese: `default`); `route(language, installed)` resolves only capabilities
of a Tesseract node, returning an `EngineRoute` or `None`. Sampling's existing
`no_model` counter includes an unavailable selected engine, not just missing
Tesseract weights. There is no Japanese fallback to Tesseract.

`boinc::Engine.kind` carries that selection through job identity, validation,
result matching and the `ocr_engine` family. It defaults to Tesseract and is
omitted when serializing Tesseract jobs, preserving existing JSON, identity
hashes and ledger tags. Tesseract-only BOINC workers reject other kinds before
checking models or starting Tesseract. The dedicated NDLOCR-Lite worker
validates `job.engine.kind`, its pinned image, version, profile and model
digest; use `usnm-boinc ndlocr stage/submit/collect`, not the native Tesseract
exporter. For on-prem contract adapters, the hello's family must match the
selected engine and its models must pass the catalog's existing production gate.
The historical survey-only NDLOCR-Lite entry remains separate from the pinned
production entry. Dashboard routes retain the engine and version as well as the
profile.
