# Native OCR without a container runtime

Issue [#1418](https://github.com/tgoodyear/usnewsmap.com/issues/1418). `usnm_ocr` and `usnm_gold` ship only as container apps, so a host without Docker or Podman can't read pages. This doc covers the Linux x86_64 build that needs no container runtime, how a host running it can earn trust, and what has to happen before any host gets it.

Nothing here is released. No app version exists, no plan class changed, and no unit has been sent to a host. Windows and macOS aren't built ([Other platforms](#other-platforms)).

## The build

A build is a set of files, and each is a BOINC app version file:

| File                     | What it is                                                                                 |
| ------------------------ | ------------------------------------------------------------------------------------------ |
| `usnm_ocr`               | The worker, `crates/usnm-ocr-app`. Static, `x86_64-unknown-linux-musl`.                    |
| `usnm_tesseract`         | Tesseract, `ocr/boinc/native/Containerfile`. Static, against glibc 2.41.                   |
| `tessdata_<model>`       | The 22 models in `ocr/engines/tesseract/models.lock`.                                      |
| `usnm_native_build.json` | The manifest: the size and sha256 of every other file. Its sha256 is the build's identity. |

It comes to 202 MB, the same models as the worker image.

**Tesseract.** The Containerfile takes the engine image's Debian snapshot and links Debian's own static libraries: `libtesseract.a`, `libleptonica.a`, OpenJPEG, libjpeg-turbo and the rest, at the versions in the worker image. Debian's `-dev` packages don't ship the command-line `main()`, so that one file is compiled from Debian's source package. The code that reads a page is therefore the code in the container, not a rebuild of it. There are two differences:

- libcurl and libarchive are replaced by `ocr/boinc/native/stubs.c`. Tesseract uses curl only for a URL input and libarchive only for a model packed as an archive, and neither happens here.
- `main()` is compiled without `HAVE_FEENABLEEXCEPT`. The container reaches the same state with `fpshim.c`, an `LD_PRELOAD` no-op, so floating-point traps are off in both.

**The worker.** A task is three processes:

1. The process the client starts keeps the control channel, reports progress, and writes `boinc_finish_called`.
2. A child in its own process group runs `crate::worker::run`, the function `usnm-boinc work` runs in the container. Pages, bundles, checkpoints and gold units use the same code, and the same files in and out.
3. `tesseract`, in that child's `PATH`, is a link back to `usnm_ocr`. It sets the page's limits and execs the bundled engine.

A suspend stops the whole group, and a quit or abort kills it. If the client kills the first process alone, the kernel kills the other two through a parent-death signal, which also reaches them while they're stopped. The group dying at once is what keeps a stopped page from being recorded as a failed one in a bundle's checkpoint.

The models stay in the project directory. They're hard-linked into the slot, or copied if the slot is on another filesystem, because `crate::read` refuses symlinks.

### Limits

| Container (`ocr/boinc/job.toml`) | Native                                                                                                                |
| -------------------------------- | --------------------------------------------------------------------------------------------------------------------- |
| `--memory=1536m`, no swap        | `RLIMIT_AS` of 1536 MiB on the engine. Address space, so stricter. The client also enforces `rsc_memory_bound`.       |
| `--cpus=1`                       | `OMP_THREAD_LIMIT=1`, one BOINC CPU, and BOINC's own priority.                                                        |
| Page deadline by the clock       | `RLIMIT_CPU` of the job's `timeout_secs` on the engine. It stops counting while the task is suspended.                |
| `--tmpfs=/tmp`, 512 MiB          | `TMPDIR` in the slot, `RLIMIT_FSIZE` of 512 MiB, and the client's `rsc_disk_bound`.                                   |
| `--pids-limit=64`                | No equivalent. `RLIMIT_NPROC` counts every process of the user, so it would break the host. The engine is one thread. |
| `--network=none`                 | No equivalent without privileges. Neither the worker path nor the static engine has network code it uses.             |
| `--read-only`, `--cap-drop=ALL`  | No equivalent beyond `no_new_privs`. The task runs as the BOINC user, as `usnm_quality` does.                         |
| `core`: default                  | `RLIMIT_CORE` of 0 and 256 open files.                                                                                |

The three rows with no equivalent are real differences. A native task runs under the BOINC account with no sandbox. That's the same position as native `usnm_quality`, and the difference is that this one runs a large C++ program over scans. The scans come from the project.

A gold unit starts again after a suspension instead of stopping mid-crop, because it reports crop times by the clock and its validator bounds them. It takes minutes and has no checkpoint to lose. It also keeps the job's deadline by the clock, as in the container, so a crop can't pass its CPU limit and then fail the validator's time check.

## A build's identity

`engine.image` in a job has to be `sha256:<64 hex>`, and trust is keyed to it. A native build has no image, so its identity is the sha256 of its manifest. The manifest lists the worker and the engine, so a change to either is a new identity. `engine.base` in a gold job is the sha256 of `usnm_tesseract`.

At the start of every task the worker hashes every file against the manifest and hashes the manifest itself. It then refuses a unit unless the job's `engine.image` is exactly this build, and for gold, unless `engine.base` is too. A job for the worker image fails without reading a page. Without this, a host could return a page under a digest it never ran.

## Its own BOINC apps

Native versions can't go in `usnm_ocr` or `usnm_gold`. They need their own apps, `usnm_ocr_native` and `usnm_gold_native`, with the same templates. From the pinned BOINC source:

- `get_app_version` picks the best version for a workunit among all that pass their plan class. Only a workunit with `app_version_num` is tied to one version, and only bundles set it. A native version in `usnm_ocr` would be offered every single-page container unit, on any host whose plan class check passes. Those jobs name the worker image, so the worker would refuse them, and each refusal is a client error.
- Plan classes can require Docker, and can't require its absence. `<docker/>` exists and nothing inverts it.
- For gold it's worse. A refused container gold unit is a `CLIENT_ERROR`, which `gold trust` treats as a revocation for that image, so a trusted host could lose its place on the worker image's list. The scheduler's gold hook would refuse the pair first, but then a host whose best version is native gets no container gold at all.

Separate apps avoid all of this. A unit can only ever reach versions of its own kind, and an existing workunit can't be affected, because it's in an app the native versions aren't in.

## Trust

Today's lists are keyed to the worker image digest, `247c…`. A native build is a different digest, so it starts with nobody in either language. An English result from an untrusted host is quarantined and stops the controller's tick ([boinc-batch-scheduler.md](boinc-batch-scheduler.md#gold-onboarding-901)). So no native OCR unit may reach a host before that host is trusted for the native digest.

What doesn't work, and why:

- **Copying hosts from `247c…` to the native digest.** That's a claim about software those hosts never ran, and the trust list records what was measured.
- **Listing the native digest as an alias of `247c…`.** The same claim, made in code, and it would hold for every later native build.
- **A separate threshold.** Nothing here calls for one, and the trust model has no notion of it.
- **Rebuilding with `gold trust --merge` from native reports alone.** Re-promoting a scope replaces it, so this would drop `247c…`'s hosts. Native hosts go in through `gold-promote`, which only adds.

What does, all of it in this change:

1. `usnm_gold_native` runs the gold unit in the native worker. Its report has the same schema and goes through the same `usnm_gold_check`.
2. The scheduler's gold hook covers the new app. In place of a Dockerfile, it requires the version's `usnm_native_build.json` to have the physical name that carries the job's image digest. The job's base must still match a reviewed entry in `gold-engine-platforms.json`.
3. `GOLD_NATIVE_HOSTS` on `gold-onboard` and `gold-promote` names the hosts. A native campaign reaches only those hosts and uses the native app. A person sets it. It isn't in the controller's allowed ssh inputs.
4. `gold-promote` judges each host against the scope's existing `max_cer` and `manifest_sha256`, and refuses a campaign that isn't the calibrated challenge. It adds hosts under the native digest and nothing else. For English that's the frozen 30-crop challenge `d8cd7279…` and the threshold 0.08734328574534386. Neither changes.

`trust/tests.rs` covers this: a pass at the threshold admits the host for the native digest only, a miss by 1e-9 doesn't, and a result from another app is refused as evidence.

Every new native build starts over, as every new worker image does.

**Threshold carry-over.** The threshold was calibrated on the container's output, and the page results below say the native build's output is the same on one machine. That isn't proof for the challenge. So before any volunteer host runs it, a project host runs the calibrated challenge on both builds. The per-crop edit counts must be identical. If they aren't, the threshold doesn't carry over and this stops for a new calibration decision.

## Parity

Measured on 1 November 2026. The per-page results are in [2026-11-01-native-ocr-parity.json](notes/2026-11-01-native-ocr-parity.json). Every page went through `usnm_ocr` and through `usnm-boinc work` from the unpacked `247c…` worker image, started at the same moment on one 8-core host under other load (load average around 25). The container arm ran under chroot with the image's environment, not under a container runtime. One German page was also read under podman with the `job.toml` limits, and its text and ALTO had the same hashes.

Both builds report Tesseract 5.5.0, Leptonica 1.84.1, libjpeg-turbo 2.1.5, libpng 1.6.48, libtiff 4.7.0, OpenJPEG 2.5.3, libwebp 1.5.0 and libgif 5.2.2. The 22 models have the hashes in the lock. CI built `usnm_tesseract` separately and got the same sha256, `21877bc3…`.

| Language and format  | Pages | Text identical | ALTO identical | CER between builds | Native pages/hour | Container pages/hour |
| -------------------- | ----: | -------------: | -------------: | -----------------: | ----------------: | -------------------: |
| German Fraktur, JPEG |    15 |             15 |             15 |                  0 |             19.97 |                20.36 |
| German Fraktur, JP2  |    12 |             12 |             12 |                  0 |             12.92 |                13.22 |
| English, JPEG        |    16 |             16 |             16 |                  0 |              4.50 |                 4.51 |
| English, JP2         |    13 |             13 |             13 |                  0 |              4.23 |                 4.17 |
| All                  |    56 |             56 |             56 |                  0 |              6.74 |                 6.74 |

- **What was compared.** ALTO by sha256, so byte for byte. Text in this run was compared after Python's newline normalization, and the script now compares raw bytes. The ALTO holds every word, and identical words mean the CER against any reference is identical too. On the 31 pages with LoC OCR on disk, the mean CER against it was 0.625 for both builds. That measures agreement with LoC's own OCR, which has a different reading order, so it isn't accuracy.
- **Speed.** Both builds spent about 245 CPU seconds a page: 243.8 native, 246.6 container. The host was loaded, so pages per hour compares the two builds and doesn't predict a volunteer's rate.
- **Which build.** 42 pages ran on the first worker build, and 14 ran on `db763b6f…`, after most of the review fixes. Both have the same `usnm_tesseract`. Two later worker changes, an engine parent check and clearing scratch before a unit starts, have only run in CI's smoke test, and a release build gets its own digest in any case.
- **Reruns.** 8 of the 56 are reruns. Two native reads were killed by mistake during testing. Six pages hit the 1200 s job deadline in one or both builds, so they ran again with a 3600 s deadline. Those 8 are the only reruns, and 6 English JP2 pages were added to the sample at the same time.
- **Gold.** A local bundle of 45 crops, 30 Fraktur and 15 Finnish, scored identically crop by crop with 0 skipped, and the native report passed `usnm-boinc gold check`. It's built from public sources on disk. It has no English and isn't the calibrated challenge, so it doesn't stand in for rollout step 3.
- **Stock client.** Debian's BOINC 8.0.4 ran `db763b6f…` against a local stand-in project, with no container runtime on the host. A plain task finished and uploaded. Suspend and resume stopped and restarted the engine. A client restart mid-bundle resumed from the checkpoint and left no engine running. A job for another build was refused. Killing the parent of a stopped task also killed the unit and the engine. A newer client hasn't been tried, and `docs/boinc-volunteer.md` recommends 8.2.13.
- **Clean host.** On a clean Ubuntu 24.04 container, the native build's output for one German page had the same hashes as the container worker's.

**This doesn't show the builds are equivalent.** It shows identical output on 56 pages on one CPU. Tesseract picks its code path by CPU feature, and glibc's `libm` does too, so output can differ between CPUs in either build. No second machine was compared. If a page ever differs on the same CPU, treat the builds as different until explained. The safe comparison then is per-crop edit counts on the calibrated challenge, plus independent-host spot checks on pages, and neither licenses calling them equivalent. Any other platform starts from that position.

### Clock and CPU deadlines

The two builds enforce a job's `timeout_secs` differently, and this changes which pages finish. The container measures the clock. The native engine has `RLIMIT_CPU`, because the clock keeps running while BOINC has a task suspended.

Under this host's load, with the usual 1200 s deadline, the first pass of 48 pages gave:

| Result at 1200 s | Pages |
| ---------------- | ----: |
| Both finished    |    42 |
| Only native      |     5 |
| Only container   |     0 |
| Neither          |     1 |

The container failed 6 pages for taking over 1200 s on the clock. Native finished 5 of them, using 497 to 1099 CPU seconds. The sixth page needs about 1370 CPU seconds in either build, so `SIGXCPU` stopped native too. All 6 are identical at the 3600 s deadline. The 2 native reads killed in testing are left out of this table.

So on a loaded host a native task gets more clock time than a container task for the same job. A native page can take far longer than `timeout_secs` by the clock if the host is busy, up to BOINC's own bounds. A page that needs more CPU than `timeout_secs` fails in both. The deadline doesn't change a page's bytes, only whether the page finishes. Gold keeps its clock deadline in both builds.

## Rollout

Each step needs the one before it.

1. **Merge this.** Nothing changes on the server. The new app names don't exist there, `publish-appver.sh` still refuses them, and `gold-engine-platforms.json` has no native entry.
2. **Register `usnm_gold_native` only.** Add it to `project.xml` with its validator and assimilator daemons, allow it in `publish-appver.sh`, `check_appver.py` and `boinc-app-release.sh`, add the reviewed binding to `gold-engine-platforms.json`, rebuild the server image for the hook, then sign and publish. No workunit exists yet, so no host gets anything.
3. **Check the challenge.** A project host with a container runtime runs the calibrated challenge on both builds, and the per-crop counts are compared.
4. **Gold for named hosts.** `gold-onboard` and `gold-promote` with `GOLD_NATIVE_HOSTS`, one host first. Gold units can't stall a tick. A failure is an alert, and a host that doesn't pass stays off the list.
5. **Build the OCR dispatch.** None of this exists yet. These are the server-side limits on native OCR work:
   - **A targeted unit only.** `usnm_ocr_native` units are created with `--target_host`, for a host chosen by the plan. Assignment is already enabled for gold.
   - **Trust checked at submission.** Submission calls `Trust::refuse_job` for that host and the job's exact scope and digest, and doesn't create the unit if it's refused. Today trust is only read at collection. This moves the refusal to before the work exists, so an untrusted host can't return a native page at all.
   - **A held native result doesn't stop the tick.** Trust can still change while a unit is out, because a rebuild can revoke it. A held native page stays held and unpublished, as today, but it pauses the native plan and raises an alert. Container work carries on. Publication rules don't change. This changes which plan stops, and the page is still held.
   - **A bound.** The native plan gets its own `max_pending` and `pages_per_host`, within the global cap.
6. **Canary.** One trusted host, 10 English pages, one at a time. Independent-host spot checks on every page (`host-trust spot`, a trusted container host reading the same page).
7. **Widen by hand.** One host at a time, by plan. German waits for its own decision (#835), on either build.

To roll back at any step, stop creating native work. Units already out finish and collect under their own digest, or stay held. Nothing touches the container apps.

## Other platforms

The design keeps Windows and macOS open, and neither is built.

- **Build identity.** `platform` is in the manifest, and the binding table is keyed by platform, so each platform is its own digest and starts with its own trust.
- **Windows.** The worker needs a job object in place of process groups and rlimits, and a copy in place of the `tesseract` link. `BoincControl` already handles the Windows channel. Debian's libraries don't apply, so Tesseract has to be built from the same versions with another toolchain and C library.
- **macOS.** Same shape as Linux, and the same toolchain point, with Mach-O and ad hoc signing as `usnm_quality` has.
- **Parity.** A different libm and compiler mean byte-identical output can't be assumed, so these need their own parity comparison.

## Reproducing

```sh
docker build -f ocr/boinc/native/Containerfile -t usnm-tesseract-native ocr/boinc/native
cargo build --release --target x86_64-unknown-linux-musl -p usnm-ocr-app --bin usnm_ocr --example native_fixture
usnm_ocr manifest --worker usnm_ocr --tesseract "$PWD/usnm_tesseract" --tessdata tessdata --out usnm_native_build.json
scripts/boinc/smoke-native-ocr.sh --build build --fixture native_fixture
scripts/boinc/native_ocr_parity.py run --manifest pages.json --pages pages --build build \
  --fixture native_fixture --rootfs worker-root --container-image sha256:... \
  --container-base sha256:... --work parity --parallel 2
scripts/boinc/native_ocr_parity.py report --work parity --references loctext --out parity.json
scripts/boinc/native_ocr_client_test.py --boinc boinc --boinccmd boinccmd --build build \
  --fixture native_fixture --scan page.jpg --language eng --scenario plain --work client
```

`.github/workflows/boinc-ocr-native.yml` runs the build and the smoke test, and keeps the build as an artifact.
