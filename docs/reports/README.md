# Reports

One directory per report, named `<year>-<month>-<topic>`. A report is `report.md` (or `README.md`), the scripts that built it, and its figures. Its data (CSV, JSON, JSONL, parquet, archives, PDFs) is not kept in the tree: it is published as a GitHub release asset and fetched back into place when someone needs it, so the tree stays prose and code.

| Report                                                      | What it is                                                                   |
| ----------------------------------------------------------- | ---------------------------------------------------------------------------- |
| [2026-10-boinc-quality-pilot](2026-10-boinc-quality-pilot/) | The `usnm_quality` scoring and `usnm_qwen` crop transcription pilot on BOINC |
| [2026-10-iris-model-reuse](2026-10-iris-model-reuse/)       | Iris Xe: model reuse and compile cache (#736)                                |
| [2026-10-jp2-vs-jpeg-ocr](2026-10-jp2-vs-jpeg-ocr/)         | JP2 against JPEG page inputs for OCR                                         |
| [2026-10-language-analysis](2026-10-language-analysis/)     | Languages in the index, October 2026 (`build.sh` renders the PDF)            |
| [2026-10-ocr-engine-survey](2026-10-ocr-engine-survey/)     | The OCR engine survey behind [ocr-gold-set.md](../ocr-gold-set.md)           |
| [2026-10-surya-volunteer](2026-10-surya-volunteer/)         | Surya 2 on BOINC volunteers                                                  |

## Data

`scripts/data-assets.sh` defines the `report-data` set as every tracked file under `docs/reports/` and `docs/data/` that is not prose or code (`.csv`, `.json`, `.jsonl`, `.parquet`, `.txt`, `.gz`, `.pdf`, `.tar`). `ops/data-assets.json` records the release that holds the current set and the tarball's sha256.

```sh
scripts/data-assets.sh --fetch --set report-data     # downloads, checks the digest, extracts under the repository root
scripts/data-assets.sh --members --set report-data   # what the set holds at this commit
```

To publish a changed set, run the `publish data assets` workflow (`.github/workflows/publish-data.yml`) on the commit that holds the files, with `set` = `report-data`. It packs the set reproducibly, creates the release `data-report-data-<date>-<commit>` with the tarball, its member list and `SHA256SUMS`, and prints the entry for `ops/data-assets.json`. A release is never rebuilt: a changed set is a new run and a new tag, and the old tag stays fetchable with `--tag`.

Writing a new report: put the data in the report's `data/` directory, publish the set, record the tag, and remove the data from the tree in the same change that cites it. The benchmark's binaries (`ocr/bench` page images and raw native results) follow the same path as the `bench-assets` set ([ocr-bench.md](../ocr-bench.md)).
