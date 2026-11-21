# Part A: Tesseract frak2021 on public German Fraktur truth

Scores `tesseract/5.5.0 frak2021` (PSM 3, 300 DPI; `deu_latf` as reference arm)
against public page ground truth, per the German Fraktur truth protocol (Part A). Python 3 stdlib
plus Pillow; `fetch.sh` needs bash, git, curl, unzip, python3.

## Run

```sh
export DATA_ROOT=/path/to/data            # each set goes in $DATA_ROOT/<set>/
./fetch.sh --list --set chronicling             # print the layout; download nothing big
./fetch.sh --set chronicling --set reichsanzeiger --set nzz # add --with-austrian for the optional set
```

Read the `--list` output and adjust `year_ok` (year filter) and the `*_select`
functions in `fetch.sh` once the layout is known; if per-page years live in a
metadata file, give a TSV `page_id<TAB>year` as `CG_YEAR_MAP`. Each set gets its
LICENSE/README files and a `MANIFEST.tsv` (sha256, path). Requests are spaced by
`SLEEP` seconds (default 1.5, minimum 1); reruns resume.

```sh
python3 score.py --data-root $DATA_ROOT --out results.csv \
  --tessdata /path/to/tessdata --workers 4 --gpu-study-pages gpu_pages.txt
python3 analyze.py --csv results.csv
python3 tests_score.py
```

- `--sets` (default `chronicling,reichsanzeiger,nzz`; add `austrian`), `--arms`
  (default `frak2021,deu_latf`), `--years 1850-1920`, `--limit N` for a smoke run.
- `--meta file.csv` (columns `page_id,title,year,stratum`) overrides what
  `score.py` guesses: year from the first 18xx/19xx in the id or path, title
  from the id before the year, stratum `ood` when a path component is `ood`
  (Chronicling Germany's 20 out-of-distribution pages), otherwise `test`.
  Pages without a year are skipped unless `--keep-unknown-year`.
- `--gpu-study-pages`: page ids (or `set/page_id`), one per line, `#` comments.
- Engine: `--cmd` is a template with `{img} {imgdir} {imgname} {model} {tessdata}`.
  Image form (entrypoint `tesseract`, models baked in at `/opt/tessdata`):
  `--cmd 'podman run --rm --network=none --user 10001:10001 -v {imgdir}:/in:ro,Z IMAGE@sha256:DIGEST /in/{imgname} stdout -l {model} --psm 3 --dpi 300' --version-cmd 'podman run --rm IMAGE@sha256:DIGEST --version'`.
  `OMP_THREAD_LIMIT=1` is already set in that image; with the bare binary set it
  yourself for comparable CPU seconds.
- `results.csv.meta.json` records the model sha256 per arm (a warning is printed
  if frak2021 is not `1da23842...e299a`), the command and `tesseract --version`.
- Output is resumable: rerun with the same `--out` and finished (set, page, arm)
  rows are skipped. Failed pages are reported and retried next run.
- CPU seconds are the `RUSAGE_CHILDREN` delta around the call (user+system). With
  `podman run` this counts the client only, not the container; use the bare
  binary, or read the cgroup, when CPU cost matters.
- The CSV holds numbers only, never OCR or GT text. Temporary PNGs (for formats
  such as jp2) are removed after each page.

## Rules

Reference: PAGE-XML `TextLine/TextEquiv/Unicode` per line, lines joined by `\n`,
regions in `ReadingOrder` (then document order); ALTO `String@CONTENT` joined by
spaces per line, a `HYP` attached to its word.

- **Raw**: NFC, CR dropped, whitespace collapsed to single spaces.
- **Folded**: ſ to s; ʒ (U+0292) to z; U+0364 to U+0308; a hyphen mark (`-`,
  `¬`, `⸗` U+2E17) after a word character and before newline plus a word
  character is dehyphenated (mark and break removed, left to right, no
  overlap, as `qe::text::normalize`); any remaining `¬` or `⸗` becomes `-`; then
  whitespace collapse. Applied to reference and OCR alike.
- **CER** = Levenshtein edits / `ref_chars`. `ref_chars` is the raw normalised
  reference length, shared by raw and folded CER (folded edits use the same
  denominator, so pooling is sum edits / sum ref_chars for both).
- **Word F1**: letter runs (inner apostrophes allowed, case kept) of the folded
  texts, bag overlap, F1 = 2 overlap / (n_ref + n_hyp).
- **Decision rule A** (`analyze.py`): pooled folded CER over the decision pool
  (Chronicling Germany 1850-1920, ood stratum included, plus Reichsanzeiger
  pages not in the GPU study); page bootstrap, 10,000 resamples, seed
  20261121; lift iff upper 95% limit <= 0.179. Any set with a point estimate
  above 0.235 is flagged (hold and investigate) and overrides a lift. A
  title-clustered bootstrap is shown alongside; GPU-study pages are reported
  separately.

## Licences

Reichsanzeiger-GT is CC0. Chronicling Germany, NZZ GT and Austrian Newspapers GT
are CC BY-NC: evaluation only. Nothing is redistributed, no derived page text or
images are published, and `fetch.sh` keeps each set's LICENSE next to its data.
Keep `$DATA_ROOT` out of any repository or release.
