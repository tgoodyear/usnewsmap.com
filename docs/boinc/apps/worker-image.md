# The worker image

Part of the [BOINC batch pilot guide](../README.md).

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

These commands install the Debian packages from the snapshot. Once
`ocr/engines/tesseract/base.lock` pins the apt bases for your architecture, pass
them to both builds to skip the mirror, as CI does
([Rebuild the apt bases](#rebuild-the-apt-bases)):

```sh
# A separate assignment, so a rejected pin, malformed lock or failed pull stops here
# instead of leaving `bases` empty and falling back to the snapshot.
lines=$(scripts/ci/tesseract-base.sh --arch amd64 --check-label docker) || exit 1
bases=()
for line in $lines; do
  bases+=(--build-arg "$line")
done
# then add "${bases[@]}" to each docker build above
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
index only after both builds pass. Pull requests build and test without pushing, and only
when the worker's own recipe changes; they build arm64 only when its Containerfile, the
engine's Containerfile, the apt bases' lock or `scripts/ci/tesseract-base.sh`, `.dockerignore`,
the workflow or its build action changes. A merge to
`main` builds both architectures without publishing (#1585). To publish:

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

### Rebuild the apt bases

The engine and worker images take their Debian packages from two apt bases per
architecture in the public GHCR package `ghcr.io/tgoodyear/usnm-tesseract-base`
(#1681), so their builds don't contact `snapshot.debian.org`:

- the engine base, the `apt-engine` stage of `ocr/engines/tesseract/Containerfile`:
  Tesseract and its libraries, which the engine image starts `FROM`;
- the tools base, its `apt-tools-base` stage: `curl` and `gcc` for the build stages
  that fetch the models and compile `fpshim.c`, and the `.deb` files of `procps`
  and the libraries it needs, which `boinc/apps/Containerfile` installs on the
  engine image with no mirror.

`ocr/engines/tesseract/base.lock` pins each by digest. The image builds pass its
lines as `ENGINE_BASE` and `TOOLS_BASE` (`scripts/ci/tesseract-base.sh`) and check
each base against the Containerfile: its label `org.usnewsmap.debian-snapshot` and
its apt sources must name `SNAPSHOT`, and its label `org.usnewsmap.apt-recipe` must
be the hash of the `apt-*` stages' text (`tesseract-base.sh --print-recipe`). A base
from another snapshot or other packages fails the build. While an
architecture's lines are empty, its builds install from the snapshot through the
`apt-*` stages, the Containerfiles' defaults. Those stages are also how a base is
built.

Rebuild the bases when `SNAPSHOT` or a package the `apt-*` stages install changes:

1. In the same pull request, empty the lock's lines: the old bases would fail the
   check, and with no pins the builds install from the snapshot until new ones are
   pinned.
2. Run the workflow on `main`:

   ```sh
   c="base-$(date -u +%Y%m%dT%H%M%S)"
   gh workflow run tesseract-base.yml --ref main -f correlation="$c"
   ```

   It builds both bases on each architecture's native runner, checks their labels and
   Tesseract version, pushes them, attests their build provenance and writes their
   package lists as an artifact. The `lock` job's summary holds the four lines for
   `base.lock`.

3. The first time only, make the package public as for the worker image above
   (github.com/users/tgoodyear/packages/container/usnm-tesseract-base/settings),
   and check an anonymous pull of each digest from a Docker logged out of GHCR.
4. Check the provenance: `gh attestation verify oci://ghcr.io/tgoodyear/usnm-tesseract-base@sha256:<digest> -R tgoodyear/usnewsmap.com`.
5. Put the four lines in `base.lock` in a pull request. Its `boinc-worker` and `ci`
   runs build both images on both architectures from the new bases.
6. Publish the worker image as usual once that merges.

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
