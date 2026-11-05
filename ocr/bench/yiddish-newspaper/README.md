# Yiddish newspaper line test set (#1479)

Step 3 of [#1479](https://github.com/tgoodyear/usnewsmap.com/issues/1479) needs 300–500 newspaper lines, transcribed by people from the LoC Yiddish titles. They'll be used to score stock Tesseract `yid`, the Jochre-trained `yidj2` and Jochre 3. Step 4, whether `yidj2` replaces `yid`, is decided on this newspaper score, not on Study 10's book score.

**Status:** pages drawn, nothing transcribed. Nothing here is ground truth yet.

| File                                                                                        | What it is                                                                                                                            |
| ------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------- |
| `manifest.json`                                                                             | The draw: 41 pages and 21 reserves in title × decade cells, the seed, 12 anchor points per page, and the sha256 and size of each scan |
| `template.jsonl`                                                                            | One empty record per drawn page: `todo` for selected pages, `reserve` for reserves                                                    |
| [`scripts/ocr-bench/yiddish_newspaper.py`](../../../scripts/ocr-bench/yiddish_newspaper.py) | `inventory`, `select`, `template`, `export` and `check`; standard library only                                                        |

The ocr-bench workflow runs `test_yiddish_newspaper.py`. It checks:

- the anchors follow from the seed;
- the template is what the script makes from the manifest;
- the template passes `check`.

## The draw

| Cell (LCCN/decade) | Title                                               | Held | Drawn | Reserves |
| ------------------ | --------------------------------------------------- | ---: | ----: | -------: |
| sn83045837/1910    | Der Morgen Zshurnal = the Jewish Morning Journal    |  118 |     5 |        3 |
| sn86063496/1910    | Yidishes Ṭageblaṭṭ = the Jewish Daily News          |   31 |     5 |        3 |
| sn86063496/1920    | Yidishes Ṭageblaṭṭ                                  |  138 |     5 |        3 |
| sn83045838/1920    | Der Morgen Zshurnal, Yidishes Ṭageblaṭ (1929 only)  |   46 |     5 |        3 |
| sn83045838/1930    | Der Morgen Zshurnal, Yidishes Ṭageblaṭ              |  157 |     5 |        3 |
| sn83045846/1930    | Der Morgen Zshurnal = Jewish Journal and Daily News |    1 |     1 |        0 |
| sn83045846/1940    | Der Morgen Zshurnal = Jewish Journal and Daily News |   26 |     5 |        3 |
| sn83045846/1950    | Der Morgen Zshurnal = Jewish Journal and Daily News |    5 |     5 |        0 |
| sn83045861/1940    | Der Tog = the Day                                   |  232 |     5 |        3 |
| sn92063919/1890    | Der Izraeliṭ = the Israelite (Providence)           |    0 |   gap |          |

That's 41 pages × 3 blocks × 3 lines = 369 target lines.

- **Titles.** These are every title whose `languages` include `yid` in the site index (`pages-v20261026-1`). The cells are each decade between a title's first and last page date.
  - Der Izraeliṭ (184 pages, 1895–1896) has no held scan, so it's a gap.
  - Morgen Zshurnal 1930s (sn83045846) holds only one page.
- **Population.** These are the Yiddish pages held by the `qwen-nonenglish-v1` BOINC campaign: pages the quality pipeline rated high-damage. Study 8's 100 crops came from the same pool.
  - That's the population the damage router sends to a Yiddish reader. It isn't all Yiddish newsprint, so report results as "damaged pages".
- **Order.** Within a cell, pages are ranked by sha256(`seed|doc_id`), taking one page per issue before a second page of any issue. The first 5 are drawn; the next 3 are reserves.
- **Anchors.** Each page has 12 anchors, in pixels of the held scan, placed by the same hash within the middle 90% of each axis.
- **Seed.** `usnm-1479-yiddish-newspaper-v1` was fixed before any page image was opened.
- **Inventory.** The inventory of 754 held pages isn't committed; its sha256 is in the manifest. To rebuild it, run this read-only on the BOINC server:

  ```sh
  nice -n 19 ionice -c3 python3 yiddish_newspaper.py inventory \
    --units <project>/qwen/qwen-nonenglish-v1/units --lang yid --out inventory.jsonl
  python3 yiddish_newspaper.py select --inventory inventory.jsonl --titles titles.json \
    --seed usnm-1479-yiddish-newspaper-v1 --out manifest.json   # titles.json: GET /api/v1/titles
  ```

## Transcribing

Who transcribes is a maintainer decision (#1479, #738). It needs two people:

- a qualified Yiddish reader who transcribes;
- a second, independent reader who reviews.

1. **Export the scans.** Run `yiddish_newspaper.py export --units <units> --manifest manifest.json --out <folder>` on the BOINC server. It copies only the drawn scans, as `<doc_id>.jpg`, and checks each sha256.
   - Never hand a transcriber the units folder. It holds Qwen's readings (`qwen.json`).
2. **Use anchors in order, starting from anchor 0.** At each anchor, find the printed line at that point. If the point falls between lines, take the next line below it in the same column.
   - That line and the next two lines below it in the same column or region form a **block**. Record the block's `region`: `body`, `headline`, `advertisement`, `caption`, `table` or `other`.
   - Record the block's `column`: `[x0, x1]` in scan pixels, the left and right edges of that column or region. It must contain the anchor and every line of the block.
   - If the first line starts more than a line's height below the anchor, or two lines of a block are more than a line's height apart, say why in the block's `gap_reason`, for example a paragraph break or a rule. `check` counts these gaps and warns.
   - Skip an anchor only for one of these reasons, recorded in `skipped_anchors`:
     - `non-text`: picture, rule, margin or blank;
     - `already-used`: it falls on a line already in a block;
     - `not-yiddish`: for example, English;
     - `unreadable`: nothing in the region can be read.
   - Stop after three blocks. Leave later anchors alone.
3. **For each line, record:**
   - `box`: `[x0, y0, x1, y1]` in scan pixels, tight around that line's print;
   - `text`: the line as printed, in logical (typing) order, following [`diplomatic-v1`](../../../docs/ocr-bench-annotation-policy.md). Keep spelling, punctuation, hyphens, ligatures (װ ױ ײ) and points as printed. Use no bidi control characters.
   - Read what this scan shows; don't fill in text from another copy of the page, and never guess words.
     - Write an unreadable stretch as one U+FFFD.
     - Mark a reading the reviewer confirms is uncertain with `illegible_spans`: code-point offsets with a `reason`.
4. **Short blocks and pages.**
   - A block with fewer than three lines (because the region ends) needs a `short_reason`.
   - A page with fewer than three blocks after all 12 anchors needs a page `short_reason`.
5. **Rejecting a page.** Only reject a page for one of these reasons, given in `reject_reason`:
   - `no-yiddish-text`;
   - `blank-or-non-text`;
   - `image-mismatch`;
   - `other`, with `notes`.

   Then change the cell's next reserve from `reserve` to `todo`. Never reject a page for damage or difficulty: damage is what this set measures.

6. **Prefill.** Set `prefill` to `none`. If anyone corrects an engine's output instead of transcribing, name the engine. `check` warns, because that engine's score is biased.
7. **Status.** `todo`, then `transcribed` (with `transcribed_by`), then `reviewed` (with `reviewed_by`, a different person, and `evidence`). Freeze the text before anyone looks at an engine's output for these pages.

A record with one block (the text and coordinates are made up):

```json
{"doc_id": "…", "status": "reviewed", "prefill": "none", "transcribed_by": "reader-a", "reviewed_by": "reader-b",
 "evidence": "independent second reading, 2026-12-01", "skipped_anchors": [{"anchor": 0, "reason": "non-text"}],
 "blocks": [{"anchor": 1, "region": "body", "column": [790, 1180], "lines": [
   {"box": [812, 1490, 1160, 1522], "text": "די נײַע שול איז געעפֿנט"},
   {"box": [812, 1524, 1160, 1556], "text": "געװאָרן אין \ufffd", "illegible_spans": []},
   {"box": [812, 1558, 1160, 1590], "text": "מיט הונדערט קינדער."}]}], "...": "other fields as in the template"}
```

Check the records with:

```sh
python3 scripts/ocr-bench/yiddish_newspaper.py check --manifest ocr/bench/yiddish-newspaper/manifest.json \
  --records <records.jsonl> [--images <folder>] [--require-complete]
```

It prints lines, blocks, characters, gaps, skipped anchors and rejections per cell. It fails on any of these:

- an anchor that was passed over;
- a first line that isn't at its anchor;
- a line outside its block's column, or a column that doesn't contain the anchor;
- a gap over a line's height without a `gap_reason`;
- a box outside the scan;
- a line that repeats another line;
- control characters;
- a review by the transcriber;
- reserves used out of rank order;
- with `--require-complete`, a cell that isn't fully reviewed.

## After transcription (not done)

- **Score** stock `yid`, `yidj2` (`ocr/models/yidj2`, release `model-tesseract-yidj2-20261105`) and Jochre 3 on line crops cut from the boxes. Jochre 3 is only for scoring; it's AGPL and isn't in production.
  - Use Study 8's scorer, which simplifies both sides, and `raw-v1`.
  - Bootstrap over pages and report by cell.
- **Then decide step 4.**

## Rights

The repository holds only LoC page ids, scan hashes and anchor points: no images and no page text. `diplomatic-v1` requires redistribution rights for both the image and the text. So before transcriptions are published as a benchmark release, confirm the rights for the 1929–1953 issues.
