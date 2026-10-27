# The worker image

Part of the [BOINC batch pilot guide](README.md).

## Build the worker image

First build the existing engine image. Keep its pinned base, package snapshot,
and `models.lock`, including `frak2021`; the BOINC worker verifies the selected
models' bytes at execution. The lock includes Spanish (`spa`), French (`fra`),
Italian (`ita`), Yiddish (`yid`, then `heb` fallback), Hebrew (`heb`), Danish
(`dan`), Norwegian (`nor`, for `nor`/`nob`/`nno`), Finnish (`fin`), Swedish (`swe`),
Lithuanian (`lit`), Slovenian (`slv`), Hungarian (`hun`) and Slovak (`slk`).
These thirteen tessdata_best additions are pinned to the same revision as the
existing Apache-2.0 models and add 126,180,270 bytes (120.33 MiB) of model data
to both images before compression. The BOINC Containerfile inherits them from
the Tesseract image; it has no separate model list.

After changing the lock, republish **both** images. Run and score a pilot per
new language with the existing quality checks (including #522) before replacing
LoC text. Yiddish also needs the reader work in #328; Danish and Norwegian
1880s–1910s may need Fraktur routing (#325). Adding models alone does not
perform these operational steps.

Then build the BOINC image using that engine image's
immutable digest (these examples run from the repository root):

```sh
docker build -f ocr/engines/tesseract/Containerfile \
  -t usnm-tesseract:pilot ocr/engines/tesseract
docker image inspect usnm-tesseract:pilot --format '{{.Id}}'

docker build -f boinc/apps/Containerfile \
  --build-arg TESSERACT_IMAGE=sha256:ENGINE_IMAGE_ID \
  --build-arg TESSERACT_FROM=usnm-tesseract:pilot \
  -t usnm-boinc:pilot .
```

Replace the placeholder with the full engine image ID. `TESSERACT_FROM` is
needed with Docker's BuildKit, which can't resolve a bare local image id in
`FROM`; podman can, and it defaults to `TESSERACT_IMAGE`. Volunteers pull the
published image ([Publish the worker image](#publish-the-worker-image)); an app
version records its registry digest,
`ghcr.io/tgoodyear/usnm-boinc-worker@sha256:FULL_DIGEST`, not the engine base's
pin. Rebuild and sign a new BOINC app version when changing code, models, image,
or runtime configuration. The Rust build uses the repository's locked
dependencies; capture the build revision and compiler in your release.

### Publish the worker image

Volunteers pull the worker image from a public GHCR package,
`ghcr.io/tgoodyear/usnm-boinc-worker` (ADR-0019, addendum). The workflow
`.github/workflows/boinc-worker.yml` builds and smoke-tests separate native
`linux/amd64` and `linux/arm64` images on matching Ubuntu runners. The Debian and
Rust bases are pinned per architecture in that workflow; no QEMU setup is used.
Publishing starts by hand from `main` and combines both tested images into one OCI
index only after both builds pass. Pull requests build and test without pushing:

```sh
c="worker-$(date -u +%Y%m%dT%H%M%S)"
gh workflow run boinc-worker.yml --ref main -f correlation="$c"
# The run list is eventually consistent: wait for the run with this correlation id.
until run=$(gh run list --workflow boinc-worker.yml --event workflow_dispatch --limit 20 \
  --json databaseId,displayTitle -q "map(select(.displayTitle | endswith(\"($c)\")))[0].databaseId // empty") &&
  [ -n "$run" ]; do sleep 5; done
gh run watch "$run" --exit-status
gh run view "$run" --json url -q .url   # the run's summary holds the digest
```

The index job's summary and outputs `digest` and `image` give the **index**
digest to use in `boinc/scripts/install.py --image` and `images.worker` in
`boinc/infra/boinc/env/<env>.json`. The summary also records each child
manifest and its `tesseract_image` engine configuration ID; the two engine IDs
differ even though their Tesseract version and model lock match.

`GOLD_ENGINE_IMAGE` and `ASSESS_ENGINE_IMAGE` use the worker index digest.
`GOLD_ENGINE_BASE` and `ASSESS_ENGINE_BASE` must match the architecture executing
the cohort. The current bridge settings hold only one base ID: keep assessment
and gold campaigns restricted to a matching architecture until per-architecture
expectations are supported. Start Mac validation with reserved `usnm_ocr` jobs;
this build support does not establish cross-architecture gold agreement.

The image is tagged with the commit; only the digest is used. A new GHCR package
is private, and a workflow can't change that. Once, after the first push:

1. Open the package's settings: github.com/users/tgoodyear/packages/container/usnm-boinc-worker/settings.
2. Under "Danger Zone", choose "Change visibility", then "Public", and type the package name to confirm.
3. Check that it is linked to this repository (the image's
   `org.opencontainers.image.source` label links it at the first push).
4. From a Docker that is logged out of GHCR, on a host that is not the maintainer's:
   `docker logout ghcr.io; docker pull ghcr.io/tgoodyear/usnm-boinc-worker@sha256:<digest>`.

Later pushes keep the package public. Verify an anonymous pull on each native
architecture before releasing Mac app versions. See the [Mac release procedure](app-release.md#mac-app-versions).

Before changing the installed worker image, stop creating new cohorts. Finish
exporting and submitting already allocated cohorts with their original image,
and drain all existing workunits through collection. Resolve uncertain and
failed tasks before rolling the app version; do not send their existing requests
to a different image. The worker checks the exact Tesseract version, profile and
model bytes, and the collector checks the result against the job's immutable
engine metadata. Installing a new app version does not rewrite existing requests
or bind `create_work` to an image automatically. Resume with fresh cohorts and
the new image pin after the old cohort is drained. Retain old signed files and
journals for recovery.

## Local synthetic demonstration

Requires Rust and the worker image above. `pilot_fixture` creates a rendered
`HELLO WORLD` page plus native manifest, sidecar, curated and reference stores,
and file-backed state. It makes no network requests and contains no production
data. Use a fresh directory:

```sh
cargo run --locked -p usnm-boinc --example pilot_fixture -- --dir /tmp/usnm-boinc-demo
cargo build --locked -p usnm-boinc

target/debug/usnm-boinc export \
  --manifest /tmp/usnm-boinc-demo/manifest.parquet \
  --pages /tmp/usnm-boinc-demo/pages.txt --batch test_pilot \
  --scans /tmp/usnm-boinc-demo/scans --root /tmp/usnm-boinc-demo/dispatch \
  --plan pilot-demo --image sha256:WORKER_IMAGE_ID \
  --curated-url /tmp/usnm-boinc-demo/curated \
  --reference-url /tmp/usnm-boinc-demo/reference \
  --state-file /tmp/usnm-boinc-demo/state.json
```

The exporter prints a job ID. Run its directory through the worker image:

```sh
docker run --rm --network=none --read-only --cap-drop=ALL --cap-add=DAC_OVERRIDE \
  --security-opt=no-new-privileges --cpus=1 --memory=1536m \
  --memory-swap=1536m --pids-limit=64 --tmpfs=/tmp:rw,size=512m \
  -v /tmp/usnm-boinc-demo/dispatch/jobs/JOB_ID:/work usnm-boinc:pilot
```

On SELinux hosts, label only this scratch directory appropriately (e.g. a `:Z`
volume). The stock spool names are `<workunit>__file_<logical_name>`; for this
local test copy the three output files into a scratch spool with that naming,
where the workunit is `usnm_JOB_ID`, then run:

```sh
target/debug/usnm-boinc collect --root /tmp/usnm-boinc-demo/dispatch \
  --spool /tmp/usnm-boinc-demo/spool --job JOB_ID \
  --curated-url /tmp/usnm-boinc-demo/curated --out-url /tmp/usnm-boinc-demo/out
```

Run it twice: publication remains one ALTO/Parquet pair. The Rust integration
suite also runs these outputs through the real `ocr-sync` in directory mode:

```sh
cargo test --locked -p usnm-boinc
python3 -m unittest discover -s boinc/scripts -p 'test_*.py'
```

The bridge explicitly unlocks its state-directory `flock` before closing it;
otherwise an unrelated fork's inherited descriptor can briefly retain the lock.
The gold command tests publish closed, synced executable fixtures by rename,
retry only fixture-spawn `ETXTBSY` (never an executed command), and verify worker
termination through a dedicated FIFO rather than sampling `ps`. The timeout
clock advances only after that worker announces its PID.
