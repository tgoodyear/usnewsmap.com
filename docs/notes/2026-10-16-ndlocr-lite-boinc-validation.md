# Japanese NDLOCR-Lite on BOINC: first full-page validation

**Status:** shipped as `usnm_ndlocr` 1.1 (app_version 26); release acceptance not recorded · **Date:** October 2026 · **Measured on:** one LoC page, `sn83025517/1945-01-01/ed-1/seq-4` (Rocky Shimpo, batch `dlc_ballston_ver01`), and the committed headline fixture; workunit 15711 · **Code:** `ocr/engines/ndlocr-lite/`, `crates/usnm-ocr-adapter`, `crates/usnm-boinc/src/ndlocr.rs`, [ndlocr-lite.md](../boinc/apps/ndlocr-lite.md) · **Issues:** #626, #699, PR #700, PR #716

## The question

Whether the `usnm_ndlocr` worker (#626) reads a real full-size Japanese page the same way the existing Python job (`ocr/ja-ocr/jaocr.py`) does, on more than one host, and whether a page can go through the real BOINC lifecycle, from submission to the curated `ocr-ja` overlay, without hand-made result files. [ndlocr-lite.md](../boinc/apps/ndlocr-lite.md) describes the app as it now stands.

## What we did

1. **Native full-page baseline, 16 October 2026.** One full page read through the Rust contract adapter outside a container, on an Apple M1 (8 logical CPUs, 16 GiB RAM) with Python 3.12 and the upstream macOS ONNX Runtime pin 1.23.2. The Linux image pins ONNX Runtime 1.26.0, so this measured the adapter, not the image. Source: [Rocky Shimpo, 1945-01-01 full scan](https://tile.loc.gov/image-services/iiif/service:sgp:sgpbatches:batch_dlc_ballston_ver01:data:sn83025517:00237288798:1945010101:0365/full/full/0/default.jpg), 5,832 × 7,582 pixels, 7,607 normalized characters.
2. **Two-host Linux headline check, 16 October 2026.** The published BOINC worker ran in Docker on two test hosts, A (i7-7700K) and B (i7-1185G7), on `ocr/ja-ocr/testdata/rocky-shimpo-1945-01-01-seq-4-headline.jpg`: one CPU, 4 GiB memory with no extra swap, no network, read-only root. No workunit was submitted. Worker manifest `sha256:1c7731046380c340b73dc4c14e47a0aff1a933815bc7f733f54681d54eb70819`, engine configuration `sha256:bf3a44e1e23b18f1c234a768939acedf38dfcf432d1a3aabf10754352fb7928a`. A first attempt under host A's host-level Podman failed before inference: AppArmor profile `containers-default-0.62.2` denied the Unix stream socket Tokio's signal handler needs (a Python `socket.socketpair()` probe reproduced it, and the kernel audit log confirmed it). No AppArmor or seccomp policy was relaxed; the run used the host's Docker test container instead, so host-level Podman compatibility is still unknown.
3. **Full-page run of the same worker, 16 October 2026.** Staged through the shipped route with the page's real `missing` classification: a 5,832 × 7,582 JPEG, 4,665,071 bytes, SHA-256 `73732eb9253253eba4269ac5afb348626bc691c9f79ecf5ddb46aae74bf4af0a`, job `41fa5b807da96634892d6b18325931ae4fc184f2c0d502f2397aa3cce11746da`. This page already had native Japanese output, so it is not a fresh-page result. Both hosts' workers failed with `ALTO/text disagree` before writing result files (#699): upstream text follows XML document order, while the ALTO converter sorts 90 blocks and their lines by `ORDER`. The headline fixture had not covered this. PR #700 kept both output formats and compared complete lines with their counts instead.
4. **Corrected worker, 16 October 2026.** #700 merged as `137306bb1574708b71c1dbf0eb70a806093f7ba7`; workflow run 38959718544 published worker manifest `sha256:60280a6b080f9c670b7218e9ba2d92bec5486ad41c8dd118e74e68f28c8dd611` with engine configuration `sha256:0a1c3fc63c183127a24a9bd49e35b917d94a6e1e5cea5e4b3349ddcd6304934a`. The staging CLI made a new job, `77a3a1457511db4ee342978a4307ee87715e12909487d9f20c60073172c4392c`, for the same page and scan. Hosts A and B ran the published worker under the same limits.
5. **Production canonical collection, 16 October 2026.** The signed Linux `usnm_ndlocr` 1.1 publish job chosen in PR #716 completed in `usnm-boinc-publish-caj-prod-cgicdf7` and registered app_version 26. Only that job's image changed. Job `77a3a145…` went through the private hand-off container; `ndlocr submit` created workunit 15711 (quorum 2, target 2). After assimilation, the canonical spool and the retained `submitted` state were copied to the publisher, and `ndlocr collect` wrote the ALTO and then a 16,719-byte Parquet part under `curated/ocr-ja/`, stem `sn83025517_1945-01-01_ed-1_seq-4.boinc-77a3a1457511db4ee342978a4307ee87715e12909487d9f20c60073172c4392c`. A collection replay succeeded without replacing the accepted bytes or timestamp. The accepted bytes, acceptance timestamp and `published` marker are kept in the private hand-off backup and on the project share under `/project/ndlocr-fullpage-77a3a145/published-state/state` (state archive SHA-256 `9552c6695eb5df22cb4fa916dc4630b1a039dc365c56f1316b9eececa8f1376c`). Don't rebuild this state from volunteer files or rerun collection from the older pre-publication snapshot.
6. **Release check, 17 October 2026 (read-only).** The four current parts for the same issue were downloaded. Three contain this page; the `.3.parquet` part holds another page.

## What we measured

**Native full-page baseline** (CPU time and peak RSS from `getrusage(RUSAGE_CHILDREN)` after the adapter reaped the engine; each run also checked against a second upstream read and the Python converters):

| Metric                                       | First run                | Repeat after contract fixes |
| -------------------------------------------- | ------------------------ | --------------------------- |
| Wall time, including startup and model load  | 40.56 s                  | 12.55 s                     |
| CPU time (user + system)                     | 65.54 CPU-s              | 48.52 CPU-s                 |
| Peak process RSS                             | 1,283,344 KiB (1.22 GiB) | 1,531,328 KiB (1.46 GiB)    |
| Text and ALTO comparison with the Python job | Exact                    | Exact                       |

Two quorum results cost roughly 97 to 131 CPU-s for a page like this one, before retries. The host was shared and not CPU-quota limited. This is one page on one host, and it set the scheduler's initial `rsc_fpops_est = 1e12` (65.54 CPU-s at an assumed 5 GFLOPS a core, with about 3× margin) and `rsc_memory_bound` of 4 GiB (about 2.7× the highest peak).

**Two-host headline check.** Both workers passed `ndlocr check`; `ndlocr compare` across the two result sets gave character-bigram Dice 1.000000 against the 0.99 floor. Both returned 246 normalized characters, text SHA-256 `bb779f06f366c89a46679cfac60377b103d8b8ebc941567101aca8c7aacf5978`. A separate upstream Python run on each host matched the text, and the Python ALTO converter matched all 16 String contents and geometry. Complete ALTO and result files differ, including processing timestamps.

| Measurement                       | Host A       | Host B       |
| --------------------------------- | ------------ | ------------ |
| Worker-reported elapsed time      | 25.637 s     | 10.670 s     |
| Independent Python wall time      | 25.123 s     | 10.807 s     |
| Independent Python CPU time       | 21.781 CPU-s | 10.806 CPU-s |
| Independent Python peak child RSS | 679,100 KiB  | 683,960 KiB  |

**Full-page run before #700.** Independent upstream runs on both hosts produced identical 7,607-character normalized text, SHA-256 `022cdf8fc80d2554d0ba40fad98e5f1424e32be4ab943ded7f6d675358c46145`. Their 630 complete text lines matched the Python ALTO converter's lines, duplicates included, in a different order.

| Independent Python measurement | Host A        | Host B        |
| ------------------------------ | ------------- | ------------- |
| Wall time                      | 79.431 s      | 59.724 s      |
| CPU time                       | 79.424 CPU-s  | 59.726 CPU-s  |
| Peak child RSS                 | 1,110,140 KiB | 1,107,948 KiB |

**Corrected worker.** Both hosts completed and passed the shipped checker, and their worker outputs passed the peer comparison with Dice 1.000000. Both returned the same 7,607-character text hash and 630 ALTO Strings. A separate upstream run on each host matched the worker's normalized text, and the Python converter matched every String's content and geometry. An ONNX Runtime affinity warning on host B was not fatal.

| Measurement                       | Host A        | Host B        |
| --------------------------------- | ------------- | ------------- |
| Worker-reported elapsed time      | 84.014 s      | 60.233 s      |
| Independent Python wall time      | 76.749 s      | 59.071 s      |
| Independent Python CPU time       | 76.345 CPU-s  | 59.074 CPU-s  |
| Independent Python peak child RSS | 1,110,676 KiB | 1,112,048 KiB |

**Workunit 15711**, from the server database:

| Result | Host | App version | Outcome | Validation | Exit status |
| ------ | ---- | ----------- | ------- | ---------- | ----------- |
| 31035  | 6    | 26          | Success | Valid      | 0           |
| 31036  | 6    | 26          | Success | Valid      | 0           |

Both results ran on the same host, so this shows the redundant BOINC lifecycle works, not consensus between independent volunteer hosts; the host A and B runs are the cross-host evidence. The canonical result is 31035 (assimilation state 2, error mask 0), and the stock assimilator wrote its three files under `results/0/`. The canonical text has the same 7,607 characters and SHA-256 as above; worker-reported elapsed time was 133.340 s. The downloaded part passes the production `usnm_ingest::ocr_ja::read_part` reader with source `usnm-ndlocr-lite`, reason `missing`, and `indexable() == true`.

**Release check.** The BOINC part is byte-identical to the collected one, SHA-256 `a55a3f72505803e0cb99d01864cc6362bab5e921c074238fe957120077fb9fe8`. Its `ocred_at`, `2026-10-16T22:07:36.345231Z`, is newer than both native copies (`2026-10-05T17:12:02.136580Z` and `2026-10-05T01:54:36.725406Z`), so it wins among these candidates. All three rows name batch `dlc_ballston` and have identical text and text hashes, so a search hit alone can't tell BOINC from native OCR. The BOINC row's engine is `ndlocr-lite/636d1cfeb1331f89f4048f416e49e23a09a714b5 default`; the native rows use `ndlocr-lite 636d1cfeb1331f89f4048f416e49e23a09a714b5`. At the time, public search served `pages-v20261010-2`, and the full rebuild for `pages-v20261017-1` (#283) was running.

None of this shows cross-host determinism between x86 and ARM, resource bounds across many pages, or a fresh page that `jaocr.py` had not read.

## What is left

- **Release acceptance.** None recorded since the 17 October 2026 release check. It needs the served release's frozen overlay manifest to contain this exact part and hash, the selected row to come from it, and Japanese and Latin searches and source credits against that release to show it.
- **Fresh-page pilot.** A small classified set not yet read by `jaocr.py`, run on independent hosts across CPU architectures, with cross-host character scores and the full-page CPU and RSS distribution, including mixed English and Japanese columns. Tune the 0.99 floor and the scheduler bounds from that pilot.
- **Host-level Podman.** The AppArmor denial on host A is unresolved.
