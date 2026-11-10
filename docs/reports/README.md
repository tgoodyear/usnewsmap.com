# Reports

Each report records what was measured, how, and what it found at the time. It keeps its findings, numbers and methods as published. Where later work changed its subject (an app retired, a recommendation carried out), a dated status line at the top says so and links to the current docs.

One directory per report, named `<year>-<month>-<topic>`, holding `report.md` (or `README.md`), the scripts that built it and its figures. The Beyond Words baseline is a single file. Data (CSV, JSON, JSONL, parquet, archives, PDFs) is published as a GitHub release asset and fetched back into place when someone needs it (see [Data](#data)). `2026-10-language-analysis/build.sh` renders that report's PDF.

| Date | Report | Finding |
| --- | --- | --- |
| 8 October 2026, follow-ups to 18 October | [2026-10-language-analysis](2026-10-language-analysis/) | The index's 23,765,561 pages span 29 cataloged languages; LoC's OCR is most damaged in Serbian, Russian and Czech, in German before the 1920s and in English from the 1840s to the 1880s |
| 12 October 2026 | [2026-10-ocr-engine-survey](2026-10-ocr-engine-survey/) | Five engines on 80 pages, scored against LoC's OCR: Tesseract `frak2021` cut German median damage from 0.371 to 0.086, and NDNP Open OCR beat LoC on all 15 of the worst English pages |
| 13 October 2026 | [2026-10-jp2-vs-jpeg-ocr](2026-10-jp2-vs-jpeg-ocr/) | Tesseract exited with status 136 on 37 of 132 JP2s; where both formats read, the text was about equally close to LoC's |
| 16 October 2026 | [2026-10-boinc-quality-pilot](2026-10-boinc-quality-pilot/) | All 225 `usnm_quality` replica tasks agreed bit for bit across macOS and Linux; ALTO word confidence doesn't track Tesseract's error (ρ = −0.054), while `legibility_ceiling` tracks LoC's (ρ = −0.761) |
| 17 October 2026 | [2026-10-iris-model-reuse](2026-10-iris-model-reuse/) | On Iris Xe, loading the model is about 70% of a task; reusing it for 8 crops gave 3.8 times the crops per minute, and adding a warm compile cache 6.4 times |
| 18 October 2026 | [2026-10-surya-volunteer](2026-10-surya-volunteer/) | All 23 Surya 2 units ran and validated on BOINC, but every GPU-assigned run reported `device=cpu` |
| 29 October 2026 | [2026-10-29-beyond-words-baseline.md](2026-10-29-beyond-words-baseline.md) | Newspaper Navigator's picture boxes against Beyond Words' 1,358 verified boxes: 77.69% precision, 41.53% recall (class-aware, IoU 0.5) |
| 6 November 2026, data of 31 October | [2026-10-ocr-damage-ranking](2026-10-ocr-damage-ranking/) | 8.1% of 23.54 million scored pages are badly damaged; 1,823,814 pages are re-OCR candidates, 91.3% of them English; plus a German Fraktur pilot list |
| 6 November 2026 | [2026-11-gpu-studies](2026-11-gpu-studies/) | Xiaomi-OCR-0 read 612 human-transcribed crops at 4.0% CER against Tesseract's 9.0%; a loop guard fixed Qwen's German losses; Surya stayed worse than Tesseract |
| 6 November 2026, updated 9 November | [2026-11-cherokee](2026-11-cherokee/) | LoC's Cherokee OCR misreads at least 8–9% of syllabary letters; `chr_phoenix`, Tesseract fine-tuned on the paper's type, read the silver test lines at 10.2% CER against LoC's 18.0% |

## Data

`scripts/ops/data-assets.sh` defines the `report-data` set as every tracked file under `docs/reports/` and `docs/data/` that is not prose or code (`.csv`, `.json`, `.jsonl`, `.parquet`, `.txt`, `.gz`, `.pdf`, `.tar`). `ops/data-assets.json` records the release that holds the current set and the tarball's sha256. The tracked [source-rights notice](report-data-rights.txt) describes mixed-source material; the accompanying `report-data-manifest.json` records the SHA-256 of every staged file except the manifest itself. The packer refuses missing, changed or unlisted files. This is an inventory check, not an automatic determination of rights.

A report can instead have a set of its own, named `report-<directory>` (for example `report-2026-11-gpu-studies`). It holds every tracked file in that report's `data/` directory and the rights notice, and those files leave `report-data`. To add one, add an entry with no tag to `ops/data-assets.json` and the set to the workflow's choices, publish it, record the printed tag and digest, and remove the data from the tree. The release tag is named for the report, and `--fetch --set report-<directory>` restores it.

```sh
scripts/ops/data-assets.sh --fetch --set report-data     # downloads, checks the digest, extracts under the repository root
scripts/ops/data-assets.sh --members --set report-data   # what the set holds at this commit
```

To publish a changed set, review the source terms and migration plan, update the notice and inventory for the complete intended archive, and verify the packer in an isolated fixture. `scripts/ops/data-assets.sh --inventory --set report-data --out DIR` writes a candidate inventory from the same cumulative inputs; review its changes and source notice before copying it into the tree. Generating hashes does not approve the contents. After approval, run the `publish data assets` workflow (`.github/workflows/publish-data.yml`) on the commit that holds the new files, with `set` = `report-data`. A set is cumulative: the pack starts from the release `ops/data-assets.json` records (its tarball, checked against the recorded digest) and lays the commit's tracked files over it. The run creates `data-report-data-<date>-<commit>` with the tarball, its member list and `SHA256SUMS`, and prints the updated `ops/data-assets.json`. Verify the downloaded release and its inventory before recording that index or removing migrated files. A release is never rebuilt: a changed set is a new run and a new tag, and an old tag stays fetchable with `--tag`. An explicit historical `--tag` is not checked against the current index; verify that release's own published checksums.

Writing a new report: put the data in the report's `data/` directory, review its rights and the cumulative inventory, publish and verify the set, record the tag, and remove the migrated data from the tree in the same change that cites it. Keep `report-data-rights.txt` and `report-data-manifest.json` tracked: both go into the next cumulative pack. The current release, `data-report-data-20261023-48fbb2a`, predates them (both were added on 6 November 2026), so it holds neither. Update links to files removed from the tree and preserve historical releases and their digests. The benchmark's binaries (`ocr/bench` page images and raw native results) are published the same way as the `bench-assets` set ([OCR bench](../ocr/bench.md)).
