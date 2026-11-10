# Reports

One directory per report, named `<year>-<month>-<topic>`. A report is `report.md` (or `README.md`), the scripts that built it, and its figures. Its data (CSV, JSON, JSONL, parquet, archives, PDFs) is published as a GitHub release asset and fetched back into place when someone needs it. The source-rights notice and inventory remain in the tree.

| Report                                                      | What it is                                                                   |
| ----------------------------------------------------------- | ---------------------------------------------------------------------------- |
| [2026-10-boinc-quality-pilot](2026-10-boinc-quality-pilot/) | The `usnm_quality` scoring and `usnm_qwen` crop transcription pilot on BOINC |
| [2026-10-iris-model-reuse](2026-10-iris-model-reuse/)       | Iris Xe: model reuse and compile cache (#736)                                |
| [2026-10-jp2-vs-jpeg-ocr](2026-10-jp2-vs-jpeg-ocr/)         | JP2 against JPEG page inputs for OCR                                         |
| [2026-10-language-analysis](2026-10-language-analysis/)     | Languages in the index, October 2026 (`build.sh` renders the PDF)            |
| [2026-10-ocr-damage-ranking](2026-10-ocr-damage-ranking/)   | Corpus-wide OCR damage ranking and the German Fraktur pilot list             |
| [2026-10-ocr-engine-survey](2026-10-ocr-engine-survey/)     | The OCR engine survey behind [ocr-gold-set.md](../ocr/gold-set.md)           |
| [2026-10-surya-volunteer](2026-10-surya-volunteer/)         | Surya 2 on BOINC volunteers                                                  |
| [2026-11-cherokee](2026-11-cherokee/)                       | Cherokee: searchable volume, OCR accuracy proxies, models tried, and a plan  |
| [2026-11-gpu-studies](2026-11-gpu-studies/)                 | GPU OCR studies and their reproduction notes                                 |

## Data

`scripts/ops/data-assets.sh` defines the `report-data` set as every tracked file under `docs/reports/` and `docs/data/` that is not prose or code (`.csv`, `.json`, `.jsonl`, `.parquet`, `.txt`, `.gz`, `.pdf`, `.tar`). `ops/data-assets.json` records the release that holds the current set and the tarball's sha256. The tracked [source-rights notice](report-data-rights.txt) describes mixed-source material; the accompanying `report-data-manifest.json` records the SHA-256 of every staged file except the manifest itself. The packer refuses missing, changed or unlisted files. This is an inventory check, not an automatic determination of rights.

A report can instead have a set of its own, named `report-<directory>` (for example `report-2026-11-gpu-studies`). It holds every tracked file in that report's `data/` directory and the rights notice, and those files leave `report-data`. To add one, add an entry with no tag to `ops/data-assets.json` and the set to the workflow's choices, publish it, record the printed tag and digest, and remove the data from the tree. The release tag is named for the report, and `--fetch --set report-<directory>` restores it.

```sh
scripts/ops/data-assets.sh --fetch --set report-data     # downloads, checks the digest, extracts under the repository root
scripts/ops/data-assets.sh --members --set report-data   # what the set holds at this commit
```

To publish a changed set, review the source terms and migration plan, update the notice and inventory for the complete intended archive, and verify the packer in an isolated fixture. `scripts/ops/data-assets.sh --inventory --set report-data --out DIR` writes a candidate inventory from the same cumulative inputs; review its changes and source notice before copying it into the tree. Generating hashes does not approve the contents. After approval, run the `publish data assets` workflow (`.github/workflows/publish-data.yml`) on the commit that holds the new files, with `set` = `report-data`. A set is cumulative: the pack starts from the release `ops/data-assets.json` records (its tarball, checked against the recorded digest) and lays the commit's tracked files over it. The run creates `data-report-data-<date>-<commit>` with the tarball, its member list and `SHA256SUMS`, and prints the updated `ops/data-assets.json`. Verify the downloaded release and its inventory before recording that index or removing migrated files. A release is never rebuilt: a changed set is a new run and a new tag, and an old tag stays fetchable with `--tag`. An explicit historical `--tag` is not checked against the current index; verify that release's own published checksums.

Writing a new report: put the data in the report's `data/` directory, review its rights and the cumulative inventory, publish and verify the set, record the tag, and remove the migrated data from the tree in the same change that cites it. Keep `report-data-rights.txt` and `report-data-manifest.json` tracked: both are included in each cumulative archive. Update links to files removed from the tree and preserve historical releases and their digests. The benchmark's binaries (`ocr/bench` page images and raw native results) follow the same path as the `bench-assets` set ([ocr-bench.md](../ocr/bench.md)).
