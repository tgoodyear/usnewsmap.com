# The bridge steps

Part of the [BOINC batch pilot guide](../README.md).

## Start a bridge step

The bridge runs one step of `boinc/images/server/bridge-job.sh` per execution,
chosen by `BRIDGE_STEP` and that step's settings. Prod's server has been on-prem since
21 October 2026 ([boinc-onprem.md](onprem.md)), and there each step runs on the
host (installed there as `usnm-boinc-step`) as:

```sh
boinc/scripts/onprem-step.sh --env prod --target bridge --step <step> [--var NAME=VALUE ...] --wait-mins N
```

`--wait-mins` is required. `boinc/scripts/start-boinc-bridge.sh` starts only the Azure bridge job
(`boinc/infra/boinc/modules/bridge-job.bicep`) of a `"server": "azure"`
environment and the hand-off job (`--target handoff`). The hand-off job's `claim`, `release`,
`dead-letter`, `deadline-retry`, `collect` and `export` can also run on the on-prem host as `onprem-step.sh --target handoff`
([boinc-onprem.md](onprem.md#hand-off-steps-on-prem)). Since 6 November 2026 prod has no Azure hand-off job (#308): `USNM_BOINC_HANDOFF_IMAGE` is out of prod's settings, and the on-prem controller runs `claim`, `collect` and `export` on the BOINC host ([hand-off steps on-prem](onprem.md#hand-off-steps-on-prem)). What follows about the job applies only to an environment whose settings still name its image. Where the job exists, its other steps run in Azure:

```sh
boinc/scripts/start-boinc-bridge.sh --env <env> --step <step> [--var NAME=VALUE ...] [--wait-mins N] [--what-if]
```

The next two paragraphs and the log query describe that Azure job, so they apply to a
`"server": "azure"` environment only.

Don't use `az containerapp job start --env-vars` for this. With any container flag, the
CLI sends an execution template holding one container named after the job, with no image
and only the variables given, so the job's own settings (`KEY_VAULT_NAME`, `DB_HOST`,
`PROJECT_DIR`, ...) are lost. The API's execution template has no volumes or mounts
either, and every step needs the project share at `/project`. The script writes the run's
settings into the job instead (`az containerapp job update --replace-env-vars`: the
template's own settings, `BRIDGE_STEP` and each `--var`; whatever a previous run set is
dropped), then starts it with no override. The next `boinc/scripts/provision-boinc.sh` restores
the template's settings. Run one step at a time.

When a waited-for execution (`--wait-mins`) or a `boinc/scripts/provision-boinc.sh` lifecycle job
fails, the script prints the execution's last 50 console lines from the stack's Log Analytics
workspace (`ContainerAppConsoleLogs_CL`; operators have Log Analytics Reader on it), polling up
to five minutes for ingestion. To read any execution's log by hand:

```sh
az monitor log-analytics query --workspace <workspace customerId> --analytics-query \
  "ContainerAppConsoleLogs_CL | where ContainerGroupName_s startswith '<execution>-' | project TimeGenerated, Log_s"
```

### Register the apps

On a `"server": "azure"` environment, provisioning runs the idempotent `--ensure-project`
lifecycle job and then `bin/xadd`. An app added to
`boinc/project/project.xml` later (`usnm_assess`, `usnm_gold`, `usnm_surya`,
`usnm_quality`, `usnm_qwen`) is registered automatically on the next provision. On the
on-prem server provisioning does not deploy the server, so run the `register-apps` step
yourself once the new image is in place. The step copies the deployed image's
`project.xml` into the project and runs `bin/xadd`; it remains available for targeted
recovery:

```sh
boinc/scripts/onprem-step.sh --env prod --target bridge --step register-apps --wait-mins 15
```

On a `"server": "azure"` environment, start the same step with
`boinc/scripts/start-boinc-bridge.sh --env <env> --step register-apps --wait-mins 15`. It is safe
to repeat. At the pinned BOINC commit (22bbebe), `tools/xadd` looks up each
`<platform>` and `<app>` by name. It inserts the ones that are missing, updates a changed
`user_friendly_name` or `min_version`, prints "already in DB" for the rest, and never
deletes or deprecates anything. A new app gets only its name from `xadd`, so the database gives it weight 1; the step's
`register-apps.php` then sets the `<weight>` from `project.xml` on each app with no app version
yet (one `xadd` just added, or one a failed earlier run added), reads it back, and fails if it
differs. It leaves the weights of apps with versions alone and reports any that differ from
`project.xml`. Adding an active app changes the feeder's startup slot
allocation: coordinate a [feeder-only refresh](feeder-refresh.md) and verify visibility as described in
[Feeder visibility](../apps/app-release.md#feeder-visibility);
neither `xadd` nor `reread_db` rebuilds that map. An identical repeat that changes no
active app does not require a restart. The step needs a bridge image with this step in it
(`boinc/scripts/build-boinc-images.sh`, then `SERVER_IMAGE` in `/etc/usnm-boinc/server.env` and a
restart of `usnm-boinc-server.service`; `boinc/scripts/provision-boinc.sh` deploys only the Azure
server).

### Current-worker OCR 1.5 publication, 17 October 2026

The native app inventory found that Linux OCR 1.3/app 20 and Mac OCR 1.4/apps
23 and 24 still pin worker `80bf`, whereas current export and scoped trust
name worker `247c131a599403645c4c96782c0556dc27f27a5d991c764b4b9565182a8c71aa`.
Publishing current-worker Gold does not update these OCR payloads. Do not
export a current-worker pilot and assume its signed OCR app matches, or
weaken the worker's identity check.

The normal prepare-only signing workflow produced immutable `usnm_ocr` 1.5
siblings with all three payload signatures verified for each platform.
Publisher repository: `crusnmprodffapzy.azurecr.io/usnm-boinc-appver`.

| Platform              | ACR build | Publisher digest                                                          | Succeeded execution suffix | Native app version |
| --------------------- | --------- | ------------------------------------------------------------------------- | -------------------------- | -----------------: |
| `x86_64-pc-linux-gnu` | `ch7a`    | `sha256:b9bf07591a265cb7b9a260221bcfd719b320cd96e204cc78ec8d7196eec70dd2` | `9rbj04a`                  |                 41 |
| `arm64-apple-darwin`  | `ch7b`    | `sha256:ccdf4a46376776ede3621be447b15e518e15e5594bdf99ad97bcd56feb07c8d2` | `sia7vgx`                  |                 42 |
| `x86_64-apple-darwin` | `ch7c`    | `sha256:74a718918a031125e5103125a1142a35533fc73efe91b525cb8899bca96ee626` | `n7hkbcb`                  |                 43 |

All pin the exact current `247c` worker; existing signed versions remain
unchanged. The desired `images.appver` pin names the final Intel sibling;
it is not a multi-platform publisher. After exact-head Copilot GREEN review,
zero threads, all CI and merge of #762, Linux, ARM and Intel were published
separately and serially. Each execution has prefix
`usnm-boinc-publish-caj-prod-` plus the suffix above and terminated Succeeded
before the next started. Normal production mutex, per-job idle preflight
(historically recorded as all-six-idle before [#856](start-guard.md#coordination-boundaries)),
infinite controller lease and expected-previous-image guards protected each
image-only update. Exact comparisons preserved every other template,
configuration and identity property; no broad stack provision was run.

Independent checks verified all three unique active version-105 / `docker`
registrations, exact `247c` Dockerfiles and three payload/signature pairs per
platform. All nine native download URLs returned anonymous HTTP 200 and
byte-identical SHA-256 hashes. Scoped global trust stayed unchanged.
Publication and registration are **not actual OCR-client acceptance**. The
[current-worker native Gold evidence and English-only scoped promotion](../../ocr/gold-set.md#current-worker-native-canonical-evidence-and-scoped-promotion)
are separate from fresh-page OCR execution, native handoff, release/search and
Intel-client acceptance. No allocation, continuous-feed activation or
backlog-policy exception is authorized by this publisher pin.

### Fresh-page Mac computation, collection and import, 17 October 2026

The normal one-page controller admitted
`sn82014782/1789-04-04/ed-2/seq-1` from base batch `kyu_frenchie`
(LoC version `kyu_frenchie_ver02`) under plan
`boinc-mac-current247c-20261017`. Four earlier ticks correctly held at
617 pending workunits against the normal target of 600. The existing managed
client had 59 successful completed results waiting to report; an
identity-guarded ordinary project update reported them, reducing pending
work to 558. The unchanged allocation/configuration and persisted controller
state then admitted exactly one page. No cap exception, task cancellation,
reprioritization, account change or client-preference change was used.

The existing Apple Silicon client fetched the page through ordinary scheduling; this was not
a targeted Gold challenge or a relabeled Linux result. Native evidence:

| Field                        | Accepted value                                                     |
| ---------------------------- | ------------------------------------------------------------------ |
| Job                          | `cae8426495087616cdf46bcadbf648406c96c1e24561b81aa99f6d7fcee6b232` |
| Workunit / canonical result  | `16738` / `32119`                                                  |
| Canonical host / app version | `1` / `42` (Apple Silicon OCR 1.5)                                 |
| Outcome / validation / exit  | `1` / `1` / `0`                                                    |
| Assimilation / error mask    | `2` / `0`                                                          |
| Canonical report SHA-256     | `d5ac93f0be4e05258affaddecb642b01e38f4caf31f4fb40dde40e0168f51b6e` |
| Input scan SHA-256           | `64c0a1122e17ced977e1540536acdd88a8fbdeff268192fb634f832de729d958` |
| Output text SHA-256          | `92d54913897d94a50dda409f1610f9c291e88dfe0a557e482ba7e2c88e4d52de` |

The canonical report names current worker `247c`, Tesseract 5.5.0 and
profile `eng` / PSM 3 / DPI 300. Global scoped trust stayed at SHA-256
`3bfc19016cf38b47455faa4ddbe8fbbe27577c410c1d8e74a2ccdd8a0a6a651b`.
Normal controller collection then completed through `spool-out` and
trust-checked handoff execution `usnm-boinc-handoff-caj-prod-71j1xrr`
(Succeeded). The durable dispatch record is `published`; the one-page
controller reports selected 1 / published 1 / collection backlog 0.
This marker means published to `ocr-out`, not to public search.

Normal pickup execution `usnm-ocrs-caj-prod-ye8ldw4` also Succeeded.
Its durable import record
`ocr-local/import/20261017T091059970936Z-2a10320b-0000.json`
records the exact BOINC page part and ALTO as `imported`. The merged part is
`ocr-local/pages/sn82014782/20261017T091059970936Z-2a10320b-0000.parquet`,
SHA-256 `ed7f2968107ced85fec4031c7d327bcf26413c3be9680bb94d4ae702a6e0e86e`.
Its one row names the expected document, node `boinc-cae842649508`,
worker `247c`, `route=boinc@r1` and 8,669 normalized characters. The
normalized text SHA-256 is
`0747212670a1df26014df567c1226180eaee373da196775990aeb55ba99c8d4b`;
the collector applies `normalize_ocr` after validating the raw canonical
text hash, so this is not the raw worker text hash above.

Stored metric-v2 scores detect English for both readings: our damage rate
is `0.20512820512820512`, versus LoC's `0.2841726618705036`, with head-word
counts 341 versus 199. These stored scores clear rule v1's 0.05 margin and
head-count test, but are an estimate: a real release scores the texts again,
checks title languages, revocations and competing candidates, and records
its actual selection. The already-running `pages-v20261017-1` rebuild
started before this import; do not assume its frozen local-OCR inputs
include the new part or restart its writer to force inclusion.

This proves fresh-page Mac computation, native canonical assimilation,
trust-checked handoff and native import. Actual release selection,
new-version search/source credits and Intel-client execution remain open.
Retain the same controller state and journals; never manufacture another
submission or reset the page to fresh.

## Server and maintainer on different hosts

The bridge has no remote-submit API. `submit` and `reconcile` must run on the
BOINC server host where the project tree and database are reachable. `export`
and `collect` can run on the maintainer's host with its existing Entra access.
The original prefetch scan cache stays there: the dispatch root's `jobs/`
already contains the input snapshots the server needs.

For a small pilot, use a manual hand-off over a trusted transfer channel:

1. Export the allocated cohort locally, stop bridge writers for that dispatch
   root, and transfer its complete `jobs/` tree into a fresh server-local dispatch
   root. Preserve file bytes and journal timestamps; do not transfer a live lock
   or regenerate requests. Keep the frozen original as a backup.
2. Make the server copy the sole writable journal. Submit using that root and
   the local BOINC project. If submission becomes uncertain, inspect and
   reconcile on the server before transferring control again.
3. Stop server bridge writers and transfer the updated complete journal tree
   back into a fresh maintainer-local root. Transfer the matching assimilator
   spool files, including any terminal error markers. Retain the server copy
   and spool until publication is confirmed. Run `collect` against this updated
   root and the copied spool, then native `ocr-sync` as usual.
4. Transfer control again only after freezing the current writer. Do not merge
   diverging journals or run both copies concurrently: local `flock` does not
   serialize separate machines. An old backup cannot establish that a job was
   never submitted; inspect the server and preserve uncertain state.

This is a manual single-writer protocol, not tested remote orchestration. Keep
journals on persistent local storage at each stage; Azure Files/SMB lock and
atomic-rename durability have not been verified. Exercise the hand-off on a
small cohort before using the bridge's submission step on the on-prem server.

## Pull from the cloud BOINC server (dev only)

`usnm-boinc cloud pull` reads finished workunits from the cloud BOINC server's assimilation API (`GET /admin/v1/assimilate`
and `GET /admin/v1/outputs/<name>`) and writes the spool the stock `batch_collect_assimilator` leaves. It is for the dev
cloud server only. Nothing in the running bridge, the timers or the scripts calls it, and `collect` is unchanged.

```
usnm-boinc cloud pull --base-url https://<dev server> --token-file <file> --spool-dir <spool> [--app <app>] --once [--no-ack] [--limit N]
```

- The admin token comes from `--token-file`, or from the `CBOINC_TOKEN` environment variable. It is never a command-line
  value and is not logged. `--app` can be repeated; without it every app is listed.
- Layout under `--spool-dir`: `results/<batch>/<workunit>__file_<open_name>` for each output, `<workunit>_error`
  holding `0x<hex>\n` for a failed workunit, `<workunit>_no_output_files` for a success with no output file to take (as the stock assimilator, when it renames none), and
  `<workunit>__host_id` with the canonical result's host id. For a success the `__host_id` file is written last, so it
  marks the workunit complete. A marker or sidecar already there with identical content is left as it is (same inode and mtime); different
  content is rewritten.
- Each file is downloaded into `<spool>/.incoming/`, checked against the listed size and MD5, fsynced, renamed into
  place and the directory fsynced. Only then is the workunit acknowledged (`POST /admin/v1/assimilate/ack`). A failed
  workunit stays unacknowledged and the run exits non-zero. A rerun skips files already in place that verify. Run one
  puller per spool.
- `--no-ack` is shadow mode: it writes the spool and never acknowledges, so the server keeps listing the same
  workunits and a later run with acknowledgement takes them.
- A file the server lists with `present: false` is never fetched or written; the others keep their names. An absent
  `present` (older server) means present. A file without an `open_name` is written as `<workunit>__file_<index>`.
- When the output reply carries `X-Content-MD5` it must equal the listed MD5 or the workunit fails; the body's MD5 is
  checked either way. The ack reply's `acknowledged`, `already_acked`, `not_ready` and `unknown` are counted and logged;
  `already_acked` is success, and `not_ready` or `unknown` make the run exit non-zero.
- Workunit and file names with a path separator, `..`, a control character or no text are refused before any file is
  made, as are two outputs with the same name.

## Hand the cloud spool off (dev only)

`usnm-boinc cloud spool-out` takes the spool `cloud pull` wrote and hands each finished single-page job to the hand-off
store with `handoff::put_spool`, which is what the bridge's `spool-out` step calls. It is for the dev cloud server only.
Nothing in the running bridge, the controller or the timers calls it.

```
usnm-boinc cloud spool-out --spool-dir <spool> --to-url <hand-off store> [--trust <trust.json>] [--once] [--interval-secs N] [--dry-run] [--redo]
```

- It reads `results/<batch>/usnm_<job id>__file_{result.json,text.txt,layout.xml}`, `usnm_<job id>_error`,
  `usnm_<job id>_no_output_files` and the `usnm_<job id>__host_id` sidecar. A job is finished when it has an error or
  no-output marker, or all three result files and the sidecar (`cloud pull` writes it last). Anything else is listed as
  incomplete and left for a later pass.
- The host id comes from the sidecar where the bridge reads MySQL. A sidecar of 0, or none (a failed workunit has no
  canonical result), means no host id, as when the bridge's query returns nothing.
- `--to-url` is the same location `handoff put-spool --to-url` takes, and the store's own credentials apply (no new
  authentication). `--trust` is put with each job as the bridge does with the share's `gold/trust.json`.
- Names under `results/` that are no job's spool file (a workunit not named `usnm_<64 hex>`, a stray file) are counted as
  `ignored` in the summary, with up to five examples on stderr, so a spool with nothing recognised shows apart from one
  with nothing to do. This holds in a dry run too.
- `--dry-run` lists what would be handed off and writes nothing, not even the ledger; it needs no `--to-url`.
- After `put_spool` returns, a ledger entry `<spool>/.handed-off/<workunit>.json` records a fingerprint of the files,
  host id and trust file. A repeat run skips a job whose fingerprint is unchanged, and hands a changed one off again. A
  crash between the put and the ledger write repeats the put, which overwrites the same objects. `--redo` ignores the
  ledger (for example after the store was reset).
- In the destination store it may delete, and only before a job's put: objects under that job's own `spool/<job id>/`
  prefix with one of the names `put_spool` writes (`usnm_<job id>__file_result.json`, `__file_text.txt`,
  `__file_layout.xml`, `_error`, `_no_output_files`) or `host_id`, which the current spool no longer produces. Without
  that, an earlier marker, result file or host id would stay and `collect` would read a stale mix (an old `_error` beside
  new result files, say). Other objects under the prefix, other jobs' objects and `gold/trust.json` are never touched.
  Removal uses the store's `list_info` and `delete_if_match`; if a store cannot do that, or an object changed meanwhile,
  the job fails with a message and no ledger entry is written.
- It never deletes or renames a spool file. The stock flow does not either at this point: `spool-out` and `collect` leave
  `results/` alone, and `archive_history.py bridge` moves drained jobs' spool files later.
- Not handled, and listed as such: OCR bundle workunits (`usnm_ocrb_*`), which need the server's journals to split a result
  into pages. A job whose files are in more than one batch directory fails, as `batch-inventory.py` refuses it.

A timer unit for a dev host, as text only (nothing is installed by this change):

```
# usnm-cloud-spool-out.service
[Service]
Type=oneshot
ExecStart=/usr/local/bin/usnm-boinc cloud spool-out --spool-dir /srv/cloud-spool --to-url <hand-off store> --once

# usnm-cloud-spool-out.timer
[Timer]
OnBootSec=2min
OnUnitInactiveSec=2min
[Install]
WantedBy=timers.target
```

## Validate gold results for the cloud server (dev only)

`usnm-boinc cloud validatord` is the validation webhook the cloud BOINC server calls for an app whose `validator_url`
points at it. It serves the gold apps (`usnm_gold`, `usnm_gold_native`) with a fingerprint reply, and `usnm_ndlocr` and
`usnm_quality` with the server's pairwise `matches` reply (below). `usnm_qwen` is not served yet. It is for the dev cloud
server only: nothing in the running bridge or the stock validators calls it, nothing is installed, and no tunnel is configured.

It is not in any default build: no worker image, native app or bridge image build passes the feature, so their `usnm-boinc`
binary does not have the subcommand. Build it where you run it, with
`cargo build --release --locked -p usnm-boinc --features cloud-validatord`. The unit tests of the service run in every
`cargo test` of the crate.

```
usnm-boinc cloud validatord --secret-file <webhook token> --state-dir <dir> --gold-dir <project gold/> \
  --base-url https://<dev server> --token-file <admin token> [--listen 127.0.0.1:8787]
```

- `POST /v1/validate/<app>` takes the server's JSON call and answers a verdict per result. The caller is authenticated by
  the `Authorization: Bearer` token the server sends (its `WEBHOOK_TOKEN` secret), compared in constant time; the token
  is read from `--secret-file` and never logged. The cloud server sends no body signature and no timestamp, so a stale
  call cannot be told from a fresh one; `call_id` is the replay control. `GET /healthz` answers `ok`.
- The service listens on loopback by default and speaks plain http. The cloud server only calls `https` URLs, so put TLS
  in front (a tunnel or a proxy); a non-loopback `--listen` needs `--allow-non-loopback`.
- Each result's one output file is fetched from `GET /admin/v1/outputs/<name>` with the admin token (`--token-file` or
  `CBOINC_TOKEN`), checked against the request's size and MD5 by the same code `cloud pull` uses (https required off
  loopback, no redirects), and handed to `gold::validate::check`, the function behind `usnm-boinc gold check`, with
  `--gold-dir` as its `--expected-dir`. Valid answers `fingerprint`, a failing check answers `invalid`, as the stock
  `script_validator --init_script usnm_gold_check` does. A result without exactly one file is `invalid` (the init script
  exits 2). A fetch that fails, or a body that does not match its MD5, answers `retry`: the stock validator reads local
  files and never has that case.
- The gold apps have no compare script, so every valid result has the same fingerprint. No `credit` is sent; the server
  computes it.
- `call_id` makes a call idempotent. The first request seen under a call id is saved (a digest, fsynced, in
  `--state-dir/calls/`) before any work, so the id belongs to that request even if it never gets a final verdict: the same
  request may run again, a different one answers 409. The verdict is stored once every result has a final one (a reply
  containing `retry` is not stored, so the next call tries again), and a repeated request gets the stored reply with no
  new work, also after a restart. Concurrent duplicates run once. A cache entry that cannot be read (anything but "not
  there") is a 500 and is left untouched. Entries are not pruned.
- Limits: the request body is at most `--max-body-bytes` (1 MiB; 413 above), 64 results of at most 16 files, and a call
  is cut off after `--timeout-secs` (15, covering the wait behind a duplicate as well as the work; the server's `<webhook_timeout_secs>` is 20 by default) with a 503 the server
  counts as a failed call.

### The pairwise apps: `usnm_ndlocr` and `usnm_quality`

These two apps run an init script and a compare script under stock `script_validator`. The service answers a call with a
verdict per result (`invalid`, `retry`, or only the `id` for a readable result) and a top-level `matches` list of
`[id, id]` pairs, never a fingerprint (the server rejects both together).

- Init, per result, with no change to the library: `ndlocr::load` (what `usnm-boinc ndlocr check` runs) and
  `quality::validate::check` (`usnm-boinc quality check`, plus the result-name rule its `--keep` applies). A result
  that does not list exactly three files, has a file over that position's read limit, or fails the check is `invalid`
  (the init scripts exit 2 on a wrong count). A fetch that fails is `retry`.
- Compare, with no change to the library: `ndlocr::compare` and `quality::validate::compare`, called as stock
  `check_set` and `check_pair` (`sched/validate_util2.cpp`) call them. `check_set`: the usable results in request
  order, each as the candidate (first argument) against every other usable result, stopping at the first candidate with
  at least `min_quorum / 2` matches; nothing is compared when fewer than `min_quorum` results are usable. `check_pair`:
  each late result (first argument) against the canonical result, and no other pair. A compare that errors is no match.
  The server decides valid and invalid from the pairs, as stock does (D39 and D41 in cloud-boinc
  `cloud/docs/compatibility.md`).
- A request for these apps needs `mode`, `workunit.min_quorum` and, for `check_pair`, a `canonical_result_id` among the
  results; otherwise it is a 400.
- Any `retry` in a `check_set` call means no comparison is run and no side effect is recorded: the server ignores
  comparisons of such a call, and the next call (a new call id) runs them. In `check_pair` each late result stands on
  its own (D41): the late results that could be fetched are compared and recorded, and the one that could not is
  `retry`. If the canonical result is not usable nothing is compared.

Side effects of the stock scripts, and what this service does with each. State lives under `--state-dir/quality/`.

| Stock side effect                                                                                                                                             | Where stock keeps it            | In this service                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `usnm_quality_check` copies every result that passes into `quality/seen/<result name>/` (`result.json`, `confusions.json`, `diag.txt`)                        | `<project>/quality/seen`        | Reproduced under `--state-dir/quality/seen/<result name>/` by the same library call (`check` with `keep`). Written only when the call has no transient failure (`check_set`) or the canonical result is usable (`check_pair`). A repeat overwrites the same bytes, so a replay or a retry adds nothing.                                                                                                                                                                                                                                         |
| `usnm_quality_compare` appends one JSON line per compare call to `quality/pairs.jsonl` (both orders appear when a `check_set` compares a pair from each side) | `<project>/quality/pairs.jsonl` | Reproduced in `--state-dir/quality/pairs.jsonl`: the line is the library's own, one per compare call as stock makes them. Each line is journaled under `quality/pairs-journal/` by call id and the two result ids: its text and the file length it goes at are saved, then it is appended and fsynced, then marked done. A repeated call id, a retried call and a restart add the line once, and a torn append is repaired. A different call id for the same results (a later revision) appends again, as stock would on a new validation pass. |
| The stock daemons write these as the validator's own user, where the bridge's collect and analysis read them                                                  | `<project>` tree                | Not moved there: the files are under `--state-dir`. Pointing the analysis at them, or copying them into the project tree, belongs to the install step (B4).                                                                                                                                                                                                                                                                                                                                                                                     |
| `usnm_ndlocr_check` and `usnm_ndlocr_compare` leave no file; `ndlocr compare` prints its Dice score to stderr                                                 | stderr of the daemon            | Nothing to reproduce. The library still prints the score, which lands in the service's own stderr.                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| `usnm_qwen_*`: `seen/`, `pairs.jsonl`, `pages/`, `crop-states`, the operator files `validator-rule` and `bundle-verdict`                                      | `<project>/qwen/`               | Later step (B3). `usnm_quality` and `usnm_ndlocr` read no operator rule file.                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| `script_validator`'s own work: credit, suspicious-result handling (`VAL_RESULT_SUSPICIOUS`), the transient exit code 3 of an init script                      | the validator daemon            | Credit is computed by the server. Suspicious-result handling has no webhook form (D39). Neither init script ever exits 3 (they read local files); a failed fetch is the `retry` case instead.                                                                                                                                                                                                                                                                                                                                                   |

Where this differs from the stock validators: a `check_set` with a `retry` result compares nothing, where stock goes on
comparing the others (D41: the server defers the call either way); the `at` time of a log line is the validator's clock
when it was first computed; a pair is unordered in the reply, so a compare function whose answer depends on its argument
order cannot be told apart; and fetching the outputs is new (stock reads local files).

A unit file, as text only:

```
# usnm-cloud-validatord.service
[Service]
ExecStart=/usr/local/bin/usnm-boinc cloud validatord --secret-file /etc/usnm/webhook-token --token-file /etc/usnm/admin-token --state-dir /var/lib/usnm-validatord --gold-dir /srv/gold --base-url https://<dev server>
Restart=on-failure
[Install]
WantedBy=multi-user.target
```
