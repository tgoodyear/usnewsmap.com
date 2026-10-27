# Iris performance experiment: measured gains, negative rollout decision

Measured 27 October 2026 for [#736](https://github.com/tgoodyear/usnewsmap.com/issues/736).
This supersedes the unscored accuracy discussion in the
[historical model-reuse report](../reports/2026-10-iris-model-reuse/README.md).
It is the original issue's explicitly permitted **no-change experiment outcome**,
not a claim that the positive rollout gates passed.

## What was rechecked

Downloaded and checksum-verified the existing
[report-data release](https://github.com/tgoodyear/usnewsmap.com/releases/tag/data-report-data-20261023-48fbb2a).
Read all 24 retained trial JSON files and their wall-time files, verified all
prediction-text hashes and distinct source-image bindings, and recovered the
whole-trial GPU/CPU telemetry. Regenerated the existing summary byte-for-byte
with `scripts/archive/2026-10/iris-bench-report.py`.

Then scored every retained prediction against its exact `(crop_id, page_sha256)`
reference from the already-verified 1,000-job input snapshot used in the
[Iris pilot acceptance](2026-10-27-iris-pilot-closeout.md). No source image,
transcription, model, scoring tolerance or worker was changed. Normalization is
whitespace collapse, preserving case/punctuation; CER is character-weighted
sequential Levenshtein, using the existing `iris-campaign.py` implementation.

The original eight-crop timing set is **one English and seven German crops**,
not the required full 100-English-crop acceptance set. Its repeated runs reuse
the same eight distinct image hashes; they are not 24 independent crops.
The 128-crop trials contain **11 English / 117 German** distinct images.
The old table's “parity vs reference” compares prediction bytes with N=1,
**not** machine OCR against human reference text.

## Actual timing, quality and memory

Whole-container elapsed time includes container startup, but not BOINC scheduler
gaps. Recovered whole-trial telemetry reproduces the historical figures:
N=1 averages about 17% GPU busy, N=8 about 55%, and warm cache + N=8 about 80%.
Pipeline load is about 24 seconds uncached versus 1.8 seconds warm.

| Retained variant                | Trials | Container crops/min | English edits / reference chars | German edits / reference chars | Peak cgroup memory |
| ------------------------------- | -----: | ------------------: | ------------------------------- | ------------------------------ | ------------------ |
| N=1, no cache, same eight crops |      3 |                 1.9 | 1 / 245 each trial              | 83 / 725 each trial            | 9.00 GiB           |
| N=4, no cache, same eight crops |      3 |                 5.2 | 1 / 245 each trial              | 85 / 725 each trial            | 8.76 GiB           |
| N=8, no cache, same eight crops |      3 |                 7.3 | 1 / 245 each trial              | 85 / 725 each trial            | 7.26 GiB           |
| Cache + N=8, cold               |      1 |                 6.8 | 1 / 245                         | 85 / 725                       | 9.00 GiB           |
| Cache + N=8, warm               |      1 |                12.1 | 1 / 245                         | 85 / 725                       | 3.70 GiB           |

Thus the measured throughput gain is real, but German CER rises from
**0.11448276 to 0.11724138**, consistently across all three N=4 and N=8
repeats. Inspecting the two changed crops against their actual references:
`pub-reichsanzeiger-lines-016` gains one error at group position 1, and
`pub-reichsanzeiger-lines-601` gains one error at position 5. The former drops
a character inside a word; the latter adds a trailing hyphen. These are actual
reference-scored regressions, not merely harmless byte drift.

All retained trials report zero OOM/oom-kill events and no empty/truncated crops.
That is **not** proof of compilation headroom: each uncached N=1 repeat reaches
the 9 GiB container limit with 3,099–4,288 `memory.events max` hits; cold
cache + N=8 also reaches that limit with one max hit. The 40-crop uncached soak
likewise peaks at 9 GiB. Warm-cache results must not hide cold-start pressure
or be used to increase the existing 9 GiB container / 10 GiB CT limits.

Larger warm-cache trials remain single repetitions:

| Crops/load | Distinct images | English edits | German edits | Container crops/min |
| ---------- | --------------: | ------------: | -----------: | ------------------: |
| 8          |             128 |            31 |        1,687 |                11.9 |
| 16         |             128 |            31 |        1,687 |                12.8 |
| 32         |             128 |            31 |        1,686 |                13.6 |
| 64         |             128 |            31 |        1,690 |                14.0 |
| 128        |             128 |            31 |        1,686 |                13.7 |

The largest individual worsened cases, relative to N=8, are German crops
`pub-reichsanzeiger-lines-521` (51 to 53 edits) and
`pub-reichsanzeiger-lines-604` (4 to 6), in both N=64 and N=128. Some other
crops improve, so a one-edit aggregate improvement at N=128 is not universal
per-crop parity. Neither the cause of the drift nor state independence is
established by deterministic decoding. No claim of KV-state leakage or a
specific numerical bug follows from these observations alone.

## Decision: no additional rollout from this experiment

The joint throughput, no-regression and repeated cold-start-headroom case is
**not met**. Use the original issue's no-change branch rather than inventing a
CER tolerance or continuing until a favorable configuration appears.

Do not promote further reuse/batch/cache settings, raise concurrency or memory,
change precision/model/token limits, or authorize a larger campaign from these
results. Keep existing immutable app versions and the current configuration
unchanged. This is a conservative experiment decision, **not** retroactive
scientific approval of the previously shipped 1.1/1.2 versions.

Historical production receipts report 100/100 successful English crops for 1.2
and CER 0.02000, equal to 1.1 but above original 1.0's 0.01975175. Those are
retained historical receipts, not newly reproduced full-100 production scores
in this audit. They do not establish original-baseline no regression.

If a future proposal revisits these settings, its positive rollout gate must
still reproduce the fixed 100-English-crop paired comparison, separate German
results, inspect all changed/worst cases, pin the complete historical image/model
identities and demonstrate repeated cold/warm safety including scheduler gaps.
The retained benchmark records runtime/plugin versions and only a historical
worker-image prefix, not a recovered full historical image/model pin. Unavailable
subphase or power/thermal counters remain unknown.

Closing the **experiment with a negative/no-change outcome** does not waive
those positive-rollout gates. There was no new scientific client, live
experiment, host pause, work-request setting, deployment or campaign dispatch.

## Audit receipts and reproduction

[Checksummed aggregate scores and reproduced table](https://github.com/tgoodyear/usnewsmap.com/releases/tag/data-iris-performance-audit-20261027)
record each trial's raw/timing hashes, exact crop/image/reference/prediction
bindings, per-crop edit counts, language denominators and memory events. No raw
human-reference or prediction text is republished.

The verified private scoring-input snapshot SHA-256 is
`2c2cc79d727911ee1d441f4c1528eba88adc48be644fc5f02acd17a1cdb71c9d`.
Benchmark release: `ocr-bench-v1.0.0`, content
`c5e38736b6e60610ef0dbe6d8296c3dc58f8a945d6e1cfea5aa47b03fb163800`,
manifest `ffd1d54147966c562d1f6e13c0fe2c2badb12c726406cbd50d61d8f8a395fe29`,
all-1000 subset `3f8972e16e61f568394aac5e2ec59591d654e5dde15a9b7b53b54e03ae31a39c`.
Obtain inputs under their existing access/license conditions; hashes do not
grant redistribution rights.

For table reproduction, download the original report-data release, verify
`SHA256SUMS`, and safely unpack only `docs/reports/2026-10-iris-model-reuse/`
into an offline directory. The nested telemetry archive contains GPU/VM samples
and duplicate wall-time files; ignore AppleDouble metadata and verify duplicate
files instead of overwriting them. Then:

```sh
python3 scripts/archive/2026-10/iris-bench-report.py --dir /path/to/offline/iris \
  > /path/to/fresh-summary.md
cmp docs/reports/2026-10-iris-model-reuse/summary.md /path/to/fresh-summary.md
```

For scoring, verify every trial/timing hash in `performance-scores.json`, bind
each crop to the exact reference/image hashes in `bindings`, verify the raw
prediction's `text_sha256`, normalize both strings as above and sum the existing
`iris-campaign.py` edit distance separately by language. The published per-crop
counts reproduce each aggregate without treating repeated or partial trials as
the full English acceptance set.
