# Iris Xe: model reuse and compile cache (issue #736)

Raw per-crop results (`*.json`, with text, tokens, per-crop timings, cgroup counters), trial wall
times (`*.time.txt`), whole-trial `intel_gpu_top` and `vmstat` samples (`telemetry.tar.gz`), and the
generated table (`summary.md`). Regenerate the table with
`python3 scripts/boinc/iris-bench-report.py --dir DIR` over the unpacked artifacts.

## Setup

- Host `pve-laptop.tjg.fyi` (Computer 9): i7-1185G7 (4C/8T), 15.8 GiB, Proxmox kernel 7.0.14-24-pve,
  Iris Xe, `intel-opencl-icd` 25.18.33578.6, OpenVINO / GenAI 2026.4.1, CPU governor `performance`.
- The pinned pilot worker image (`1322b6f92481`, the one BOINC was running) executed `bench.py` inside CT 110
  with the worker's limits (`ocr/boinc/iris/job.toml`: read-only root, no network, 9 GiB, tmpfs
  `/tmp`, plus `--cpus=2`). `bench.py` and `contract.py`/`worker.py` were bind-mounted; the image was not rebuilt.
- BOINC was stopped for the whole session (the single paused task finished first); no other workload
  ran. BOINC was restarted afterwards (`systemctl start boinc-client`, run mode `auto`).
- 8 distinct, SHA-256-checked crops taken from the pilot units already staged on the host (40 distinct
  crops available; the 40-crop run is the memory soak). The operator's reference dataset is not on this
  host, so **CER against references was not computed**; see "Accuracy".
- Order of N=1/4/8 was rotated across three repetitions; the cache runs followed.

## Where the time goes (baseline)

From the six completed BOINC tasks on this host: `load_ms` 23.9-26.9 s, `generate_ms` 9.7-10.3 s
(66-74 tokens), `elapsed_ms` 33.8-37.2 s, and BOINC elapsed 39-50 s. Pipeline construction is about
70% of the worker's time, and 12-15 s more is container/wrapper startup.
Inside one process, a later crop's `generate()` takes 4.3-5.6 s (p50), not about 10 s: the first
call also pays one-time warm-up (first-crop mean 7.3-9.6 s). Mean GPU busy is 17% over a whole N=1 trial,
with an average actual frequency of 213 MHz, matching the bursty graph: the GPU idles during the
CPU-side 24 s load.

## Results (8 crops per trial; end-to-end includes container start)

| Variant | Cache | Crops/load | Trials | Load ms | Gen ms later crops p50/p95 | Crops/min | Trunc/empty/OOM | Peak cgroup | GPU busy |
|---|---|---:|---:|---:|---|---:|---|---|---:|
| Current worker path | off | 1 | 3 | 24,193 | first call 7,306 | 1.9 | 0/0/0 | 9.00 GiB (limit; reclaim, no OOM) | 17% |
| Reuse | off | 4 | 3 | 24,028 | 4,801 / 5,610 | 5.2 (2.7x) | 0/0/0 | 8.76 GiB | 39% |
| Reuse | off | 8 | 3 | 23,417 | 4,802 / 5,585 | 7.3 (3.8x) | 0/0/0 | 7.26 GiB | 55% |
| Compile cache, 8 fresh pipelines, one process | cold, then warm | 1 | 1 | 29,150 first, 1,706 warm | n/a | 5.0 cold / 7.3 warm | 0/0/0 | 8.52 / 4.40 GiB | 39% / 50% |
| Fresh process, warm cache | warm | 1 | 4 x 1 crop | 1,756-1,786 | n/a | 5.3-8.4 | 0/0/0 | 3.4-3.7 GiB | 24-57% |
| Cache + reuse | warm | 8 | 1 | 1,785 | 4,286 / 5,011 | 12.1 (6.4x) | 0/0/0 | 3.70 GiB | 80% |
| Soak: reuse, 40 crops | off | 8 | 1 | 23,802 | 5,290 / 7,663 | 6.8 | 0/0/0 | 9.00 GiB at load | 58% |

Notes: a cold cache costs the first load about 29 s and writes 3.18 GB (`cache_bytes`);
warm loads take about 1.7 s, 14x faster. Cache-only (N=1, warm) already gives 3.8x
the baseline. Within the soak, cgroup memory after each pipeline release rose 1.4 -> 2.5 -> 2.9 GiB then stayed at
2.97 GiB for the last three groups, so it was bounded (it includes page cache) over five load/release cycles.
The 9.00 GiB figure is `memory.peak` during compilation: the same pressure as the existing worker, with `memory.events`
`max` hits but `oom_kill 0`. With a warm cache the peak is 3.4-4.4 GiB, so the cache also removes the
compilation memory spike that forced the 9 GiB limit (not yet shown for a cold first run in production).
Single trials (cache rows, soak) give no variance estimate; N=1/4/8 repetitions agreed within a few percent.
The 8 crops/min "fresh process" figures include docker start but are only 1 crop per process.

## Accuracy

`generate()` uses `do_sample=False`. Across N=1 repetitions outputs were byte-identical except
one crop in one repetition (a one-token flip). Against the first N=1 trial, N=4, N=8, and all cache runs differed on
the same two crops, only when they were not the first crop in a group: 2 characters of 1,016
whitespace-normalized reference characters (0.2%), e.g. "Auswirkungen" -> "Auswirungen" and a trailing hyphen. This is
the same pattern across positions, groups and repetitions, so a mild dependence on earlier GPU/pipeline state
(or an f16 near-tie) cannot be ruled out. These measurements do **not** show CER regression or improvement
because no ground truth was available. Required before any release: score N=1 vs N>1 against the fixed
100-crop references with `iris-campaign.py` (`normalized_cer`), and inspect the two crops above.

## Decision

- Gain: N=8 is 3.8x and warm cache plus N=8 is 6.4x on this host, far above the 25% threshold; scheduler gaps
  and container startup (12-15 s per BOINC task, not measured separately here) are additional.
- Not yet accepted: reference-scored CER parity, and the multi-crop BOINC contract (v2 job/result with per-crop
  checksums, errors, partial completion) with tests and a new signed app version. The compile cache additionally needs a bounded
  writable BOINC-mounted directory keyed by model/runtime/device and a no-trust policy for volunteer-supplied blobs.
- Suggested order: (1) score references; (2) implement multi-crop workunits of 8 crops; (3) evaluate the cache as a separate
  release (3.18 GB per cache; the 8-crop soak shows reuse alone is already 3.8x without any writable mount).
- Not tested: batching, prefetch, stream/latency properties, thermal counters, German vs English split (all 8 crops
  were from the staged 100-crop pilot units, which were not separated by language here).
