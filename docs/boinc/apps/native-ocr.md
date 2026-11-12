# Native OCR without a container runtime

Issue [#1418](https://github.com/tgoodyear/usnewsmap.com/issues/1418). `usnm_ocr` and `usnm_gold` ship only as container apps, so a host without Docker or Podman can't read pages. This doc covers the Linux x86_64 build that needs no container runtime, how a host running it can earn trust, and what has to happen before any host gets it.

`usnm_gold_native` 1.0 is published for `x86_64-pc-linux-gnu` (rollout step 2). `usnm_ocr_native` is registered and its dispatch is built (#1780, rollout step 5), but no native OCR unit has been sent to a host. CI also builds macOS. Windows isn't built yet, but the release tooling accepts `windows_x86_64` ([Native usnm_ocr on more platforms](#native-usnm_ocr-on-more-platforms)).

## The build

A build is a set of files, and each is a BOINC app version file:

| File                     | What it is                                                                                                                         |
| ------------------------ | ---------------------------------------------------------------------------------------------------------------------------------- |
| `usnm_ocr`               | The worker, `crates/usnm-ocr-app`. Static, `x86_64-unknown-linux-musl`.                                                            |
| `usnm_tesseract`         | Tesseract, `boinc/apps/native/Containerfile`. Static, against glibc 2.41.                                                          |
| `tessdata_<model>`       | The 23 models in `ocr/engines/tesseract/models.lock`.                                                                              |
| `notice_<model>_<file>`  | A model's notice and licence, which ship with it: `chr_phoenix`'s `NOTICE.md` and `LICENSE-Apache-2.0.txt` (Apache-2.0 section 4). |
| `usnm_native_build.json` | The manifest: the size and sha256 of every other file. Its sha256 is the build's identity.                                         |

It comes to 202 MB, the same models as the worker image. The notices are the image's `/opt/tessdata/notices/` too: CI fetches them from the model's release and checks them against its `SHA256SUMS`, whose hash `ocr/engines/tesseract/Containerfile` pins (`ARG CHR_PHOENIX_SUMS`). `usnm_ocr manifest` reads them from `<tessdata>/notices/<model>/`, and the worker (`native::Manifest::validate`) and `boinc/scripts/check-native-ocr-build.py` refuse a build without them or with other bytes.

**Tesseract.** The Containerfile takes the engine image's Debian snapshot and links Debian's own static libraries: `libtesseract.a`, `libleptonica.a`, OpenJPEG, libjpeg-turbo and the rest, at the versions in the worker image. Debian's `-dev` packages don't ship the command-line `main()`, so that one file is compiled from Debian's source package. The code that reads a page is therefore the code in the container, not a rebuild of it. There are two differences:

- libcurl and libarchive are replaced by `boinc/apps/native/stubs.c`. Tesseract uses curl only for a URL input and libarchive only for a model packed as an archive, and neither happens here.
- `main()` is compiled without `HAVE_FEENABLEEXCEPT`. The container reaches the same state with `fpshim.c`, an `LD_PRELOAD` no-op, so floating-point traps are off in both.

**The worker.** A task is three processes:

1. The process the client starts keeps the control channel, reports progress, and writes `boinc_finish_called`.
2. A child in its own process group runs `crate::worker::run`, the function `usnm-boinc work` runs in the container. Pages, bundles, checkpoints and gold units use the same code, and the same files in and out.
3. `tesseract`, in that child's `PATH`, is a link back to `usnm_ocr`. It sets the page's limits and execs the bundled engine.

A suspend stops the whole group, and a quit or abort kills it. If the client kills the first process alone, the kernel kills the other two through a parent-death signal, which also reaches them while they're stopped. The group dying at once is what keeps a stopped page from being recorded as a failed one in a bundle's checkpoint.

The models stay in the project directory. They're hard-linked into the slot, or copied if the slot is on another filesystem, because `crate::read` refuses symlinks.

### Limits

| Container (`boinc/apps/job.toml`) | Native                                                                                                                |
| --------------------------------- | --------------------------------------------------------------------------------------------------------------------- |
| `--memory=1536m`, no swap         | `RLIMIT_AS` of 1536 MiB on the engine. Address space, so stricter. The client also enforces `rsc_memory_bound`.       |
| `--cpus=1`                        | `OMP_THREAD_LIMIT=1`, one BOINC CPU, and BOINC's own priority.                                                        |
| Page deadline by the clock        | `RLIMIT_CPU` of the job's `timeout_secs` on the engine. It stops counting while the task is suspended.                |
| `--tmpfs=/tmp`, 512 MiB           | `TMPDIR` in the slot, `RLIMIT_FSIZE` of 512 MiB, and the client's `rsc_disk_bound`.                                   |
| `--pids-limit=64`                 | No equivalent. `RLIMIT_NPROC` counts every process of the user, so it would break the host. The engine is one thread. |
| `--network=none`                  | No equivalent without privileges. Neither the worker path nor the static engine has network code it uses.             |
| `--read-only`, `--cap-drop=ALL`   | No equivalent beyond `no_new_privs`. The task runs as the BOINC user, as `usnm_quality` does.                         |
| `core`: default                   | `RLIMIT_CORE` of 0 and 256 open files.                                                                                |

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

Today's lists are keyed to the worker image digest, `247c…`. A native build is a different digest, so it starts with nobody in either language. An English result from an untrusted host is quarantined and stops the controller's tick ([boinc-batch-scheduler.md](../server/batch-scheduler.md#gold-onboarding-901)). So no native OCR unit may reach a host before that host is trusted for the native digest.

What doesn't work, and why:

- **Copying hosts from `247c…` to the native digest.** That's a claim about software those hosts never ran, and the trust list records what was measured.
- **Listing the native digest as an alias of `247c…`.** The same claim, made in code, and it would hold for every later native build.
- **A separate threshold.** Nothing here calls for one, and the trust model has no notion of it.
- **Rebuilding with `gold trust --merge` from native reports alone.** Re-promoting a scope replaces it, so this would drop `247c…`'s hosts. Native hosts go in through `gold-promote`, which only adds.

What does, all of it in this change:

1. `usnm_gold_native` runs the gold unit in the native worker. Its report has the same schema and goes through the same `usnm_gold_check`.
2. The scheduler's gold hook covers the new app. In place of a Dockerfile, it requires the version's `usnm_native_build.json` to have the physical name that carries the job's image digest. The job's base must still match a reviewed entry in `gold-engine-platforms.json`.
3. `GOLD_NATIVE_HOSTS` on `gold-onboard` and `gold-promote` names the hosts. A native campaign reaches only those hosts and uses the native app. A person sets it. It isn't in the controller's allowed ssh inputs. Of the named hosts, `gold-onboard` skips any that no `usnm_gold_native` version could be sent to, by platform, plan class and engine binding ([boinc-batch-scheduler.md](../server/batch-scheduler.md#gold-onboarding-901)). While the app has no live version that packages the campaign's build, it skips all of them.
4. `gold-promote` judges each host against the scope's existing `max_cer` and `manifest_sha256`, and refuses a campaign that isn't the calibrated challenge. It adds hosts under the native digest and nothing else. For English that's the frozen 30-crop challenge `d8cd7279…` and the threshold 0.08734328574534386. Neither changes.

`trust/tests.rs` covers this: a pass at the threshold admits the host for the native digest only, a miss by 1e-9 doesn't, and a result from another app is refused as evidence.

Every new native build starts over, as every new worker image does.

**The build challenge.** A build is also trusted only after a project machine has run the calibrated challenge through it under the threshold, and that run is recorded ([Trust per build](#trust-per-build)). The rule used to be that the build's per-crop edit counts had to equal the container's. Linux met it. macOS didn't, and the threshold now applies to every build directly.

## Trust per build

Decided on 12 November 2026, after the macOS arm64 build gave different output from Linux on every page tested but a challenge CER of 0.0719 against Linux's 0.0716 ([#1418](https://github.com/tgoodyear/usnewsmap.com/issues/1418#issuecomment-6832303647)). Output is identical within a build digest. Across digests it is identical only where `boinc/project/native-build-trust.json` lists the two as equivalent. Each build must also pass the calibrated challenge.

**The unit of trust.** The build digest: `engine.image`, the sha256 of the build's manifest. The manifest names its platform, so every platform's build has its own digest. `gold/trust.json` already keys hosts by digest within each scope, and that doesn't change. A host trusted for one digest is not trusted for another, including an equivalent one.

**Gold for each digest.** A gold unit scores a build against the challenge's reference transcriptions, never against another build's output, so no platform needs gold data of its own. Each digest gets its own campaign: `gold-onboard` with `GOLD_ENGINE_IMAGE` and `GOLD_ENGINE_BASE` of that build and `GOLD_NATIVE_HOSTS` naming its hosts. A campaign's directory under `host-trust/onboard/` is named by the hash of the prepared challenge, the engine (digest and base) and the scope, so each digest's attempts, reports and promotion records are kept apart.

**How a host earns trust for a digest.** As on Linux today. The host runs one `usnm_gold_native` unit of the scope's frozen challenge (for English, the 30 crops of `d8cd7279…`). `usnm_gold_check` validates the report, and `gold-promote` adds the host under that digest if its overall CER is at or under the scope's `max_cer` (0.08734328574534386), with no crop skipped or lost. Nothing has to match another host or another build.

**The challenge gate.** Before a digest is released, bound or trusted, a project machine runs the calibrated challenge through it, and the result goes into `boinc/project/native-build-trust.json`. It records the build's platform, `engine.base` and CI source (run, commit, artifact sha256), and for each run: the challenge manifest, the crops, the total character edits and reference characters, the threshold it was judged against, and where the evidence is. CER is edits over reference characters. A run counts only for the challenge and threshold it was judged against, so a recalibration needs a new run. The file is reviewed like any other change, and it is enforced in five places:

- `boinc-app-release.sh` refuses to release `usnm_ocr_native` or `usnm_gold_native` for a build without a passing run (`check-build-trust.py`).
- `test_check_build_trust.py` fails if `gold-engine-platforms.json` binds a native build (an entry for one platform) that isn't recorded with a passing run for that platform and base.
- `gold-onboard` and `gold-promote` refuse a native campaign whose build has no passing run on the scope's own challenge and threshold, or whose recorded base isn't the campaign's.
- `gold trust`, rebuilding the list, admits nobody for a digest whose evidence came from `usnm_gold_native` unless that digest is recorded with a passing run.
- `usnm-boinc submit --app usnm_ocr_native` refuses the unit (`Trust::refuse_native_job`), even for a host the list names.

The file is compiled into `usnm-boinc` (`crates/usnm-boinc/src/trust/builds.rs`), so a change takes effect when a server image built from it runs. Worker images aren't in it: a container host still earns trust from its own gold against the same threshold, as before.

**Pairing.** No native result is paired with another. `usnm_ocr_native` and `usnm_gold_native` have `target_nresults` 1 and BOINC's trivial validator or `usnm_gold_check`. Every unit is assigned to one host (`--target_host`) and pinned (`--app_version_num`) to the version that packages its build, so BOINC never compares two native results. The only comparison of two results is `host-trust spot`, which now compares text only when both results' `engine.image` is one digest or a listed pair, and fails otherwise.

BOINC's homogeneous redundancy isn't used. Its classes key on the host's OS and CPU vendor, not on our digest, so it can't keep two builds apart: two Macs on different builds share a class. The app version is exact, because one native app version packages one manifest for one platform. If native units ever get a quorum above 1, the app needs BOINC's `homogeneous_app_version` flag, so that every replica of a unit runs the same version, and the pin stays. `test_check_build_trust.py` fails first if `target_nresults` changes.

**A platform with one trusted host.** Publication needs no peer: as on Linux today, a trusted host's result is published on that host's own trust. Spot checks are what's lost. `host-trust spot` needs a second host trusted for the same digest, or for a listed equivalent, and refuses without one. Until a platform has two trusted hosts, its plan stays at canary size, and its pages are spot-checked by hand on a project machine running the same build, where bytes do compare.

**Results from different digests for one page.** They aren't compared byte for byte, and they aren't merged. Each is judged on its own host's trust. The page's canonical result is the one imported into the curated ledger first: a later job for the same page is marked superseded and not published (`import` in `crates/usnm-boinc/src/lib.rs`). The published row's `ocr_engine` tag carries the build digest, so a reader can tell which build produced it. A comparison across non-equivalent digests uses CER against a reference, as the challenge does.

**Linux and the container.** The Linux native digests and the worker images are comparable only where `native-build-trust.json` lists the pair, with its evidence. It lists `4d6e2a3b…` with `247c…` (identical edit counts on all 30 challenge crops) and with `dac9…` (10 English JP2 pages with byte-identical text and ALTO, on an Intel and an AMD host), and `c353f7bf…` with `247c…` (identical challenge edit counts). Pairs aren't transitive. Nothing else changes for Linux: both native builds have their recorded runs, and host 13 stays trusted for `4d6e2a3b…`.

**macOS.** The arm64 build `78065a0a…` from run 43155081356 is recorded with its run: 5957 edits in 82794 characters, CER 0.0719, under 0.0873. It isn't bound in `gold-engine-platforms.json` and no host is trusted for it. Binding it, releasing it and running gold for a named host are the next steps, in that order. The x86_64 build `dd7a112f…` hasn't run the challenge, so it can't be released.

**Windows.** The same path: CI builds it, a project machine runs the challenge, the run is recorded, then the build is bound, released and gets gold for named hosts. Nothing in the code names a platform except the list of four in `builds.rs` and `check-build-trust.py`, which already has `windows_x86_64`.

## Parity

Measured on 1 November 2026. The per-page results are in [2026-11-01-native-ocr-parity.json](../../notes/2026-11-01-native-ocr-parity.json). Every page went through `usnm_ocr` and through `usnm-boinc work` from the unpacked `247c…` worker image, started at the same moment on one 8-core host under other load (load average around 25). The container arm ran under chroot with the image's environment, not under a container runtime. One German page was also read under podman with the `job.toml` limits, and its text and ALTO had the same hashes.

Both builds report Tesseract 5.5.0, Leptonica 1.84.1, libjpeg-turbo 2.1.5, libpng 1.6.48, libtiff 4.7.0, OpenJPEG 2.5.3, libwebp 1.5.0 and libgif 5.2.2. The 22 models the lock held then have its hashes (`chr_phoenix`, the 23rd, came later). CI built `usnm_tesseract` separately and got the same sha256, `21877bc3…`.

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
- **Stock client.** Debian's BOINC 8.0.4 ran `db763b6f…` against a local stand-in project, with no container runtime on the host. A plain task finished and uploaded. Suspend and resume stopped and restarted the engine. A client restart mid-bundle resumed from the checkpoint and left no engine running. A job for another build was refused. Killing the parent of a stopped task also killed the unit and the engine. A newer client hasn't been tried, and `docs/boinc/volunteer/README.md` recommends 8.2.13.
- **Clean host.** On a clean Ubuntu 24.04 container, the native build's output for one German page had the same hashes as the container worker's.

**This doesn't show the builds are equivalent.** It shows identical output on 56 pages on one CPU. Tesseract picks its code path by CPU feature, and glibc's `libm` does too, so output can differ between CPUs in either build. No second machine was compared on 1 November. On 12 November ten pages matched byte for byte on an Intel and an AMD host, which is the evidence for the listed pair `4d6e2a3b…` and `dac9…` ([Trust per build](#trust-per-build)). If a page ever differs on the same CPU, treat the builds as different until explained, and remove the pair. Any other platform starts with no pair.

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

1. **Merge the build and the release tooling.** Done, and nothing changed on the server: the new app names didn't exist there yet, and `gold-engine-platforms.json` had no native entry. `boinc-app-release.sh`, `check_appver.py` and `publish-appver.sh` accept both apps on `x86_64-pc-linux-gnu` with no plan class. A release takes CI's build directory with `--build-dir` and first checks it with `check-native-ocr-build.py`: the manifest's Tesseract must be the version the container engine installs, its models exactly `models.lock`, and every file the one the manifest lists. The check prints the build's `engine.image` and `engine.base`. `docs/boinc/apps/app-release.md` has the command.
2. **Register `usnm_gold_native` only.** Add it to `project.xml` with weight 0 and its validator and assimilator daemons, add the reviewed binding to `gold-engine-platforms.json` (the identity the check printed), rebuild the server image for the hook, then sign and publish with `boinc-app-release.sh --build-dir`. Until the app is in `project.xml`, publishing fails at registration. No workunit exists yet, so no host gets anything. The repository side (`project.xml`, the daemons in `boinc/images/server/lib.sh`, the binding) is merged (#1607).
   - **Why weight 0.** `gold-onboard` assigns every native gold unit to one host, and the scheduler sends assigned work straight from the database (`sched_assign.cpp` at BOINC 22bbebe), not from the feeder's cache. A zero weight gets one cache slot and no query budget, and leaves the other apps' budgets where they were: a weight of 1 would then have made the sum 25 and cut each weight-1 app's budget to 7,864, under the 8,192 floor in `docs/boinc/apps/app-release.md`. `gold submit` only makes container units, so no unassigned native unit can be made.
   - **Binding.** The first native entry in `gold-engine-platforms.json` is the build from `boinc-ocr-native.yml` run 42290314510 on main `f2f4a9d0` (artifact `sha256:4435a4ce…`): `engine.image` `sha256:c353f7bf…`, `engine.base` `sha256:21877bc3…` on `x86_64-pc-linux-gnu`. `check-native-ocr-build.py` printed the same pair for the downloaded artifact. Any other build has another digest, so it needs its own entry, and its own trust. The second entry is the build from run 43155081356 on main `c622865e` (artifact `sha256:361d9aed…`), the first release of `usnm_ocr_native`: `engine.image` `sha256:4d6e2a3b…` with the same `engine.base`. Its worker has the macOS changes and the 23rd model, so it is a new digest, and `usnm_gold_native` gets a version of it for host trust.
   - **Feeder.** Adding an active app changes the feeder's startup slot map, and neither `xadd` nor `reread_db` rebuilds it (#1462). Run `register-apps` with the new `SERVER_IMAGE` before restarting the server on it, so the restarted feeder starts with the new app. `xadd` inserts a new app at the database's default weight of 1, so the step's `register-apps.php` then sets the weight `project.xml` declares on each app with no app version yet, and fails if it doesn't read back. Check `SELECT name, weight FROM app WHERE deprecated=0` shows `usnm_gold_native` at 0 before the restart, and that `feeder-slot-guard.py` reports `match` after it.
3. **Check the challenge.** A project machine runs the calibrated challenge through the build, and the run goes into `boinc/project/native-build-trust.json` ([Trust per build](#trust-per-build)). It must be at or under the threshold. Done for `c353f7bf…` and `4d6e2a3b…` on 12 November (CER 0.0716 for both, with the same per-crop counts as the container). From now on this comes before the release in step 2, because the release refuses a build without it.
4. **Gold for named hosts.** `gold-onboard` and `gold-promote` with `GOLD_NATIVE_HOSTS`, one host first. Gold units can't stall a tick. A failure is an alert, and a host that doesn't pass stays off the list.
5. **Build the OCR dispatch.** Built (#1780), and off until a plan names a host. A native plan is a queue-fed controller plan with `"app": "usnm_ocr_native"`, its own state directory, and `image` the native digest ([batch-scheduler.md](../server/batch-scheduler.md#native-ocr-plans-1780)). These are the server-side limits on native OCR work:
   - **Registered at weight 0.** `usnm_ocr_native` is in `project.xml` at weight 0, with BOINC's trivial validator and `batch_collect_assimilator`, as `usnm_ocr` has. Its units are always assigned, so a zero weight leaves the other apps' query budgets as they are, as for `usnm_gold_native`. Run `register-apps` before the server restart (#1462). The feeder takes at most 10 live apps (`MAX_APPS` in the pinned `sched_shmem.h`) and exits at start past that, so check `SELECT COUNT(*) FROM app WHERE deprecated=0` is at most 9 first. `xadd` inserts every app live, so `register-apps.php` now also deprecates an app `project.xml` declares deprecated if it has no app version, and never makes one live again. Before this, a new project got 11 live apps.
   - **A targeted unit only.** `usnm_ocr_native` units are created with `--target_host`, for a host chosen by the plan. `usnm-boinc submit --app usnm_ocr_native` refuses a unit with no `--target-host`, and so does the bridge's `NATIVE_UNITS`. Line jobs, retries and bundles stay container work. Each unit is pinned (`--app_version_num`) to the newest version the feeder loads (not deprecated, and at or above `min_version`, or else the highest of its platform; read from the database at submission) that packages the job's build (its `usnm_native_build_<digest>.json`), and with none it's refused, so a newer build's worker never gets a job for an older one.
   - **Trust checked at submission.** Submission calls `Trust::refuse_job` for that host and the job's exact scope and digest, against the bridge's `gold/trust.json`, and creates nothing if it's refused: nothing is staged and the job stays reserved. Collection still checks trust too. An untrusted host can't be sent a native page at all.
   - **A held native result doesn't stop the tick.** Trust can still change while a unit is out, because a rebuild can revoke it. A native result a collect attempt leaves unpublished, or keeps under a held scope, stays as it is, and pauses the native plan with a `native_paused` alert for as long as it is there. A refused or failed submission pauses it the same way. The tick goes on, and container plans are separate plans, so they carry on. Publication rules don't change. `--resume-native` clears a pause from a submission; a result still held, or kept and not yet archived, pauses it again.
   - **A bound.** The native plan has its own `max_pending` and `pages_per_host`, and its units count under the global cap like every plan's.
   - **CPU fill.** Each host gets as many units as its client reports CPUs (`host.p_ncpus`, from `batch-inventory`'s `host_cpus`), at most `pages_per_host`, less its units not yet settled. The host row doesn't record the client's own CPU limit (`max_ncpus_pct`), so `pages_per_host` is the bound for a host that runs fewer. A host not seen for a day, or an inventory from an older server image, gets nothing.
   - **Off by default.** An empty `native_hosts` sends nothing. The list is outside the allocation digest, so a host is added or removed by hand with no migration.
6. **Canary.** One trusted host, 10 English pages, one at a time. Independent-host spot checks on every page (`host-trust spot`, a trusted container host reading the same page).
7. **Widen by hand.** One host at a time, by plan. German waits for its own decision (#835), on either build.

To roll back at any step, stop creating native work. Units already out finish and collect under their own digest, or stay held. Nothing touches the container apps.

## Native usnm_ocr on more platforms

On 11 November 2026 the volunteer CPUs were about 5% busy. Over 6 hours they had about 600 CPU-thread-hours and ran about 31 hours of CPU tasks. `usnm_ocr`, the bulk CPU app, has container versions only, so a host without Docker or Podman gets no OCR. The idle hosts were a Linux host with 24 threads and no Docker, and three Windows hosts with 24, 16 and 16 threads.

Linux already has its build (above) and needs only the dispatch: `usnm_ocr_native` isn't registered, and nothing creates its units (rollout step 5). CI (`boinc-ocr-native.yml`) now builds macOS for arm64 and x86_64, and neither is released yet. Windows is a follow-up, [#1779](https://github.com/tgoodyear/usnewsmap.com/issues/1779).

### The worker on each platform

Every platform runs the same worker, `usnm_ocr` from `crates/usnm-ocr-app`, which runs `crate::worker::run` against the job files the container uses (`request.json`, a bundle, or a gold job). The submit side doesn't change. A native unit is a container unit whose `engine.image` names the native build's digest, and it goes to `usnm_ocr_native` or `usnm_gold_native`, never to `usnm_ocr`.

| Platform              | Worker                       | Tesseract                                                        | Supervision                                     |
| --------------------- | ---------------------------- | ---------------------------------------------------------------- | ----------------------------------------------- |
| `x86_64-pc-linux-gnu` | static musl                  | Debian's static libraries (`boinc/apps/native/Containerfile`)    | process group, parent-death signal, rlimits     |
| `arm64-apple-darwin`  | Mach-O, ad hoc signed        | pinned upstream sources (`boinc/apps/native/macos-sources.lock`) | process group, a parent watcher thread, rlimits |
| `x86_64-apple-darwin` | Mach-O, ad hoc signed        | the same sources, built on an Intel runner                       | as arm64                                        |
| `windows_x86_64`      | PE32+ x86-64 (not built yet) | the same sources with MSVC and a static CRT (planned)            | a job object (planned)                          |

**Plan class.** None, as on Linux. A task uses one BOINC CPU (`OMP_THREAD_LIMIT=1`, one page or bundle at a time), and the client runs as many tasks as the host lets it, so a 24-thread host can run 24. An `mt` class would need an OpenMP Tesseract, which the parity run didn't measure, and gains nothing when the client already fills every CPU with one-thread tasks. `check_appver.py` refuses a plan class on these apps.

**No closed class.** The `min_wu_id`/`max_wu_id` cohort mechanism is for Qwen's GPU classes. Native OCR has other gates. Its apps are separate, so no container unit can reach a native version. Every unit is assigned to one host. Submission checks that host's trust for the build, and the gold hook refuses a build with no binding in `gold-engine-platforms.json`. A published macOS version with no binding gets no work.

### Identity and validation

`platform` is in the manifest, and the worker refuses a manifest for any platform but its own (`native::PLATFORM`). So each platform's build has its own digest, `engine.image`, and its own `engine.base`, the Tesseract executable's sha256. Nothing cross-validates. `usnm_ocr_native` has `target_nresults` 1, and trust is per digest, so a macOS result is never compared byte for byte with a container or Linux result to accept it. A macOS host earns trust for the macOS digest through native gold, as a Linux host does for the Linux digest ([Trust per build](#trust-per-build)).

Off Linux, output identical to the container's can't be assumed:

- **macOS** uses upstream sources without Debian's patches, Apple's clang and libm, no OpenMP, libjpeg-turbo without SIMD, libtiff with only zlib and JPEG, and no GIF. The scans are JPEG and JP2. It is built from the same versions as the Linux build: Tesseract 5.5.0, Leptonica 1.84.1, libjpeg-turbo 2.1.5, libpng 1.6.48, libtiff 4.7.0, OpenJPEG 2.5.3, libwebp 1.5.0.
- **Windows** will add MSVC and its C runtime.

So before any host gets a platform's build, that build needs its own run of the calibrated challenge, at or under the threshold and recorded in `native-build-trust.json`. A parity run against the container worker on the same pages shows whether the build can be listed as equivalent to anything. Without an equivalence it is still trusted on its own digest. macOS arm64 has no equivalence: 0 of 14 pages were identical to Linux.

### macOS

`boinc/apps/native/build-tesseract-macos.sh --out DIR --work DIR` checks out each source at its locked commit and refuses a tag that has moved. It builds static libraries into its own prefix with CMake, ignoring Homebrew's, and links one executable. Then it checks that the executable loads only `/usr/lib` and `/System` libraries, is a Mach-O executable of the machine's architecture, and reports Tesseract 5.5.0. The deployment target is macOS 11.0. CI builds arm64 on `macos-15` and x86_64 on `macos-15-intel`, each on its own architecture, because Tesseract's CMake picks its SIMD files from the host processor.

The worker differs from Linux in three places:

| Linux                                          | macOS                                                                                                                                                                                                                                                                                                                            |
| ---------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Parent-death signal on the unit and the engine | A thread in the unit kills the group once the task's parent has gone. While the group is stopped, the parent's death orphans it and the kernel sends its stopped members `SIGHUP` and `SIGCONT`, which ends them. The client also kills a task's descendants. An engine whose unit alone died keeps running until its CPU limit. |
| `RLIMIT_AS` of 1536 MiB on the engine          | Not set. macOS doesn't enforce it, so the client's `rsc_memory_bound` is the only memory bound.                                                                                                                                                                                                                                  |
| `no_new_privs`                                 | No equivalent.                                                                                                                                                                                                                                                                                                                   |

`RLIMIT_CPU`, `RLIMIT_FSIZE`, the open-file limit and `RLIMIT_CORE` are the same. CI runs the `native::` tests and the smoke test on both architectures. Neither has run under a real macOS BOINC client.

### Windows

Planned in [#1779](https://github.com/tgoodyear/usnewsmap.com/issues/1779). The release tooling is done: `install.py`, `check_appver.py`, `publish-appver.sh`, `boinc-app-release.sh` and `check-native-ocr-build.py --platform windows_x86_64` accept the platform, and the check requires the worker and the engine to be PE32+ x86-64 executables, not DLLs. The worker port, the Tesseract build and the CI job aren't written. A job object with `KILL_ON_JOB_CLOSE` replaces the process group and the parent-death signal. Its per-process memory, CPU time and process count limits replace the rlimits, and they also cover the container's `--pids-limit`. The worker runs `usnm_tesseract.exe` directly rather than through a link. Tesseract comes from the same pinned sources, built with MSVC and a static CRT. The issue rejects UB-Mannheim's installers, which are prebuilt and ship many DLLs, and vcpkg, which would be a second way to pin the same libraries.

### Release on each platform

Each step needs the one before it. Steps 2 to 6 are the rollout above, repeated for each platform's digest.

1. Merge the build and the release tooling. `install.py`, `check_appver.py`, `publish-appver.sh`, `boinc-app-release.sh` and `check-native-ocr-build.py --platform` accept `x86_64-pc-linux-gnu`, `arm64-apple-darwin`, `x86_64-apple-darwin` and `windows_x86_64` for both apps. This changes nothing on the server.
2. Parity on a project-controlled machine of that platform: pages through the native build and the container worker (`native_ocr_parity.py`), then the calibrated challenge. Record the run in `native-build-trust.json`, and an equivalence only if the pages are byte-identical.
3. Add the build's binding (the identity `check-native-ocr-build.py` prints) to `gold-engine-platforms.json`, and rebuild the server image. The image must also contain the build's record, since `usnm-boinc` reads it at compile time.
4. Publish `usnm_gold_native` for the platform with `boinc-app-release.sh --platform <platform> --build-dir <artifact>/build`, then run gold for named hosts with `GOLD_NATIVE_HOSTS`.
5. Build the native OCR dispatch (rollout step 5, [#1780](https://github.com/tgoodyear/usnewsmap.com/issues/1780)), then register and publish `usnm_ocr_native`.
6. Canary on one trusted host, then widen by hand.

## Reproducing

Once `boinc/apps/native/base.lock` pins the apt base, the build needs no Debian mirror: pass the
`NATIVE_BASE` argument that `scripts/ci/apt-base.sh --app native --check-label docker` prints
([Rebuild the apt bases](worker-image.md#rebuild-the-apt-bases)). Without it the build installs
from the snapshot.

```sh
docker build -f boinc/apps/native/Containerfile -t usnm-tesseract-native boinc/apps/native
cargo build --release --target x86_64-unknown-linux-musl -p usnm-ocr-app --bin usnm_ocr --example native_fixture
usnm_ocr manifest --worker usnm_ocr --tesseract "$PWD/usnm_tesseract" --tessdata tessdata --out usnm_native_build.json
boinc/scripts/smoke-native-ocr.sh --build build --fixture native_fixture
boinc/scripts/native_ocr_parity.py run --manifest pages.json --pages pages --build build \
  --fixture native_fixture --rootfs worker-root --container-image sha256:... \
  --container-base sha256:... --work parity --parallel 2
boinc/scripts/native_ocr_parity.py report --work parity --references loctext --out parity.json
boinc/scripts/native_ocr_client_test.py --boinc boinc --boinccmd boinccmd --build build \
  --fixture native_fixture --scan page.jpg --language eng --scenario plain --work client
```

`.github/workflows/boinc-ocr-native.yml` runs the build and the smoke test, checks and stages the build for both apps as a release would (everything but the signatures), and keeps the build as an artifact.
