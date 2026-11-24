# Tesseract 5.5.0 SIGFPE on held Chronicling America JP2s (#2122 pick pilot, 24 Nov 2026)

**Summary.** On the #2122 pick pilot, 6 of 20 held English JP2s crash Tesseract with SIGFPE, status 136.

**Pages.** All 6 are JP2 from `/tank/ocr/scans/buffer/<batch>/...`. Their sha256 values are in `sel/pilot.tsv`.

| doc_id | Batch | Pixels | JP2 resolution header (Pillow) |
|---|---|---|---|
| sn82014141_1858-06-10_ed-1_seq-4 | iahi_charizard | 7199×8387 | none |
| sn83021207_1832-06-27_ed-1_seq-4 | mb_circe | 5303×7003 | 310 |
| sn83025581_1759-08-03_ed-1_seq-1 | nhd_bondcliff | 4682×7176 | 400 |
| sn83025588_1826-10-31_ed-1_seq-1 | nhd_avalon | 6313×9797 | 400 |
| sn83030214_1919-01-15_ed-1_seq-8 | dlc_brown | 6716×8389 | none |
| sn85025815_1804-12-21_ed-1_seq-4 | vi_alpina | 3862×6152 | 300 |

**Reproduction.** Tested on CT 107 and CT 111: Tesseract 5.5.0, Leptonica 1.84.1, OpenJPEG 2.5.3, one thread.

```sh
OMP_THREAD_LIMIT=1 tesseract <page>.jp2 out -l eng --psm 3 --dpi 300 tsv     # -> Floating point exception, rc 136
python3 -c "from PIL import Image; Image.MAX_IMAGE_PIXELS=None; Image.open('<page>.jp2').save('p.png')"
OMP_THREAD_LIMIT=1 tesseract p.png out -l eng --psm 3 --dpi 300 tsv          # -> rc 0 on all 6 (1,760 to 12,126 words)
```

**What it is.**
- The crash is in Tesseract's own JPEG 2000 path, as Leptonica and OpenJPEG decode the file.
- The same pixels, decoded by Pillow (its own OpenJPEG) and passed to Tesseract as a PNG, read normally.
- It does not depend on the JP2's resolution header: pages with none, 300, 310 and 400 all crash.
- 14 of the 20 pilot JP2s read without a crash.

**Production.**
- The worker already handles this (#454). `crates/usnm-ocr-worker/src/worker.rs` passes the raw JP2 to Tesseract.
- On status 136 (`Exited::is_status_136`), the worker reads LoC's IIIF JPEG of the page once (`jpeg_fallback`).
- So production loses no page to this, but costs a LoC fetch on each one, and reads a lossy JPEG instead of the JP2.

**For Compute.**
- 30% of a random English sample, across 1759 to 1919, is a high rate for the JPEG fallback.
- Option: decode the JP2 locally, as Pillow does, before reading or as the first fallback. Fetching the IIIF JPEG would then not be needed.
- Before deciding, check how often `jpeg_fallback` is true in production.
