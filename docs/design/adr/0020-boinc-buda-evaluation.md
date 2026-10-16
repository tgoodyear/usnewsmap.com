# ADR-0020: Retain separate BOINC apps instead of BUDA

- **Status:** Accepted; do not adopt BUDA
- **Date:** 2026-10-16
- **Extends:** [ADR-0019](0019-boinc-server-hosting.md)
- **Issue:** [#481](https://github.com/tgoodyear/usnewsmap.com/issues/481)
- **Evidence:** [evaluation note](../../notes/2026-10-14-buda-evaluation.md), [Mac standalone tests](../../notes/2026-10-14-buda-mac-offline-tests.md), [native ARM worker tests](../../notes/2026-10-14-buda-native-arm64.md) and [remaining spike procedure](../../experiments/2026-10-14-buda/README.md)

## Context

The project has three stock BOINC Docker-wrapper apps: `usnm_ocr`, `usnm_assess` and `usnm_gold`. BUDA could replace their signed app versions with one `buda` app, using science-app variants for the worker files. This would reduce release administration, but changes how science code is authorized, how results are validated and how pending tasks select worker files.

The review used the repository's pinned BOINC commit, `22bbebe08e2eaac5d5e88a6d93b88e800cb6341c`. The published version 22 Mac wrapper was inspected and launched natively on an arm64 Mac. A follow-up installed BOINC 8.2.15, verified the existing Podman installation and ran standalone tasks through the stock Mac helper: native ARM payload execution and emulated OCR passed, while cold standalone building and paused-task restart exposed limitations. There was no access to the project server, database or signing host. These observations are not an end-to-end BUDA result.

## Decision

Retain the three separate BOINC apps; do not register BUDA as an experimental or production app and do not migrate the bridge. At the pinned BOINC commit, BUDA has no per-variant validator or immutable worker-version selection, and its science files are workunit inputs rather than signed app-version files. Those are material gaps against the project's separate validation policies and offline code-signing trust boundary. A custom validator dispatcher and versioned variant names could address some behavior, but neither restores the signing boundary nor has been implemented and tested.

1. **Keep validation selected by the existing apps.** The pinned BUDA variant schema has no validator field and the setup uses `sample_trivial_validator`, which does not compare scientific output. The OCR pilot currently uses this validator too; the fuzzy OCR validator in #444 is a separate requirement, not an already deployed control. Assessment and gold use different `script_validator` checks. BUDA's documented model does not preserve these policies per variant.
2. **Do not treat a possible dispatcher as a verified replacement.** Stock `script_validator --app buda` can run a project-owned dispatcher. Passing `result_id` could let it resolve the result's workunit in the server database and select an allowlisted science-app policy. That dispatcher must reject missing or unknown mappings and compare only results from the same immutable science configuration. This integration is neither implemented nor tested; starting multiple validators with `--app buda` does not create per-variant routing.
3. **Preserve offline authorization of executable inputs.** BUDA variant Dockerfiles and `job.toml` files are workunit inputs, not signed app-version files. A digest-pinned `FROM` pins a base image but does not authenticate the Dockerfile or runtime configuration, either of which can change the command, networking or mounts.
4. **Require immutable worker identity.** The scheduler reads current variants and adds their files when sending a task; a mutable variant is not pinned into each queued workunit. Replacing it can send pending or replacement results to different code. A migration would need to journal the science-app identity, image digest, platform/variant policy, batch ID and workunit name, and prove retries and crash recovery preserve that identity.
5. **Do not count a pinned base image as a registry-pull-only path.** A variant can use a digest-qualified GHCR `FROM`, but the wrapper still builds a derived image on the volunteer host. BUDA does not enforce digest pinning, and cold/warm bandwidth and build time have not been measured in a scheduled task.
6. **Treat platform support separately.** Standalone native ARM Mac OCR and output checks passed in the local test, but no Mac task was scheduled or signed for distribution. The pinned scheduler requires a BUDA app version for the Mac client's primary platform and does not fall back to Intel on ARM. The same wrapper can be used by separate apps, so BUDA is not required to add Mac support. The Mac helper's pause/restart failure remains unresolved independently of BUDA.

## Alternatives

- **Migrate immediately using the trivial validator.** Rejected: it discards the assessment and gold checks and does not meet the anonymous-volunteer OCR validation requirement.
- **Use one BUDA app with a project-owned validation dispatcher.** Feasible in principle using stock BOINC, subject to a server-side identity mapping and the remaining tests. It does not by itself restore science-code signing or freeze mutable variants.
- **Extend upstream BUDA with per-science-app validation and immutable versions.** A possible later project. Keeping script names in web-editable variant metadata is not sufficient; policy selection must remain administrator-controlled.
- **Share the upstream wrapper binary across the three signed apps.** Keeps the current validator separation and signed Dockerfile/configuration. Improve release tooling independently of a BUDA migration.

## Consequences

The project continues signing worker Dockerfiles and runtime configurations per app version. This costs release work but preserves the current bridge and validation boundaries.

The decision is based on the pinned-source review and local standalone tests, not a live BUDA deployment. No BUDA app was registered as part of this evaluation, and web submission, remote submission, custom validation, signed distribution and scheduled end-to-end tasks were not tested because server and signing access were unavailable. The public project hostname also did not resolve from the evaluation environment, so current volunteer attachment state is unknown; PR #500 records a test-client attachment, and the issue's original zero-volunteer statement is not a verified current count. This repository change does not alter production apps or daemons. Keep #481 open for the blocked live acceptance checks; reconsider BUDA only if its validation, authorization and worker-identity gaps are resolved and tested.
