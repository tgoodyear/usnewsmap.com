# What bounds the quality feed's supply

**Status:** measured; nothing changed · **Date:** 5 November 2026 · **Measured on:** CT 211's scan index and source log from 00:00 UTC on 1 November to 12:00 UTC on 5 November 2026; the image pass's manifests and their `fetched` markers; `usnm-quality-follow`'s journal in CT 215 for the 24 hours to 12:00 UTC on 5 November; `usnm-quality-ship`'s journal on pve-laptop to 13:11 UTC; `quality.unit_pages` and `quality.staging`, read in a read-only transaction; LoC's pacer file in CT 211, read at 12:28:52 and 12:59:03 UTC on 5 November · **Code:** `scripts/boinc/onprem/quality-feed/quality_feed.py` (`follow`), `scripts/boinc/onprem/ocr-feed/ocr_manifest.py` (`build-quality`), `crates/usnm-ocr-worker/src/prefetch.rs` · **Issues:** #1475, #1465, #1415

## The question

Since about 09:00 UTC on 3 November, `usnm_quality` has run out of unsent units: volunteers finish units faster than `follow` makes them. #1475 asks what limits the pages `follow` feeds, and whether it could feed more without more LoC requests. The options were to feed the scans already held first, to feed the worst pages first (#1415), and to stop the retries of scans that don't decode (#1465). [The full image pass](../boinc-quality.md#the-full-image-pass-and-the-piggyback-962) describes the mechanism.

## What we found

**The feed takes every page the fetcher writes and has no backlog.** In the 13 hours to 12:00 on 5 November, `follow` put 21,447 pages into chunks, 1,650 an hour, while the fetcher wrote about 1,580 an hour. Of the 417,878 full-resolution scans in the index, all but 85 have a unit or a staging record, are on `follow`'s staged list, or are in its own done and skipped lists. The 85 are the 2 scans that fail to decode and pages fetched in the minutes before the read. So the supply is the fetch rate.

**After 12:00 on 5 November, the shipper fell behind.** In the 13 hours to 12:00 it shipped each chunk in about a minute and sent 1,660 pages an hour, as many as `follow` made. From about 12:08, each chunk took 2 to 16 minutes, mostly waiting for the BOINC server's step lock. It sent 207 pages from 12:00 to 13:00. From 12:49 the outbox held its 8 chunks, so `follow` waited for room, and the shipper, not the fetcher, bounded the feed. The cause wasn't looked into.

**There is no held backlog to feed first.** The index `follow` reads covers the buffer and the keep set. The 20,000 full JPEGs of the Iris experiment on CT 211 are all on the staged list, and 19,985 of them have units. Feeding held scans first, or the worst pages first, changes the order of the pages, not their number. `follow` already takes every one in each round, and the pass already queues each title and decade's worst issues first.

**The fetch rate comes from two queues.** The pass queues 2,000 pages of bucket-served batches every 2 hours 12 minutes, 909 an hour. The fetcher finished each of the 16 pass manifests queued from 01:25 on 4 November to 10:32 on 5 November between 37 and 108 minutes after it was queued (median 57), always before the next one. It spends the rest of each cycle on the `queue-quality-z368-1buffer` backlog: 1,658 manifests not yet fetched. From 12:00 on 2 November, the pass's pages from the bucket ran at 650 to 1,050 an hour. The pages from LoC, from all queues together, ran at 150 to 370 an hour.

**The rise to about 1,570 an hour from 18:00 on 4 November came from the bucket, not LoC.** The z368 manifests reached batches the bucket serves, which added 400 to 610 pages an hour. It began in the block from 18:00, before the pacer's slow rate ended at about 23:00. LoC's pages didn't rise after the slow rate ended: 215 and 232 an hour in the two blocks after, against 146 to 372 in the four before.

**LoC's budget wasn't the limit when measured.** The pacer has been at its normal 20 requests a minute since its slow rate ended. In the half hour from 12:29 on 5 November it let through 579 requests an hour, under half of that, and had free slots. The fetcher took 179 pages from LoC in that time, 1.6 requests a page.

**The retry loop costs log lines, not supply.** The deployed `follow` is #1453's (commit `b943a17a`), without #1465's quarantine. In the 24 hours to 12:00 on 5 November it logged `page failed; will retry` 2,012 times, all for 2 buffer scans fetched on 28 October that Pillow can't read (`broken data stream when reading image file`). A failed page doesn't hold its chunk back, and the round takes every other page. `follow` on main with `--quarantine-decode` stops after 3 failures and leaves the 2 pages for review (`test_follow_skips_a_quarantined_fingerprint_before_filling_its_outbox`).

**The volunteers are waiting.** At 12:11 UTC on 5 November, `usnm_quality` had 0 unsent units and 19 in progress.

**15,645 held scans are on the staged list but have no unit or staging record.** All were fetched in October. `usnm_quality` has no `v1-wave7` or `v1-wave8` campaign, though `scripts/data/build-quality-assessment-db.py` lists both. So these are probably those waves' pages, staged before the on-prem feed, whose results were never loaded. That isn't verified. Another 61,709 scans are in `follow`'s done or skipped lists but have no unit or staging record yet. This note didn't look into them.

## Method

None of the reads changed anything. Each read a bounded tail of a log, or ran a `SELECT` in a read-only transaction.

- **Fetch rate and origin.** In CT 211, the last 200 MB of `/tank/ocr/scans/index-full.jsonl` and 40 MB of `sources-full.jsonl`. Each index row is counted by its `fetched_at`. Its origin is the bucket if the page has a source record (`ndnp-batches`) since 1 November, a mirror if it has any other source record, and LoC if it has none. A page is the pass's if it is in CT 215's `/var/lib/usnm-ocr-manifest/quality-docs.txt`, OCR's if it is in `ocr-docs.txt`, and other otherwise. Rates are 6-hour counts divided by 6, rounded down. An origin under one page an hour is left out, so a row's columns can add up to a little less than its total.
- **Pass cycle.** The modification times of `manifest.parquet` and `fetched` in each `/tank/ocr/manifests/queue-quality-<stamp>`.
- **Feed rate and retries.** `journalctl -u usnm-quality-follow -o json --since` in CT 215: the pages of each `chunk ready`, and the failures by page and error.
- **Shipping.** `journalctl -u usnm-quality-ship -o short-unix` on pve-laptop: the pages of each `submitted` line by hour, and each chunk's time from its first line to `submitted`.
- **Held scans.** The index's full-resolution rows against `quality.unit_pages` and `quality.staging`, `follow`'s `--staged` list, and its `done.txt` and `skipped.txt`.
- **Pacer.** `sent` in `/var/lib/usnm-loc/pacer.json`, read twice.
- **BOINC.** The OCR dashboard.

## Results

Pages fetched an hour, by 6-hour block (UTC) and origin:

| Block        | Total | Pass, bucket | Pass, LoC | Other, bucket | Other, LoC | OCR | Mirrors |
| ------------ | ----: | -----------: | --------: | ------------: | ---------: | --: | ------: |
| 1 Nov, 00:00 |   389 |          336 |         0 |             0 |          0 |  48 |       3 |
| 1 Nov, 06:00 |   525 |          524 |         0 |             0 |          0 |   0 |       1 |
| 1 Nov, 12:00 |   152 |          149 |         0 |             0 |          3 |   0 |       0 |
| 1 Nov, 18:00 |   427 |          426 |         0 |             0 |          0 |   0 |       1 |
| 2 Nov, 00:00 |   563 |          529 |         0 |             0 |          0 |  31 |       0 |
| 2 Nov, 06:00 |   497 |          477 |         0 |            19 |          0 |   0 |       0 |
| 2 Nov, 12:00 |  1031 |          808 |        78 |             0 |        139 |   0 |       4 |
| 2 Nov, 18:00 |  1216 |          984 |        15 |             0 |        216 |   0 |       0 |
| 3 Nov, 00:00 |  1173 |          884 |        20 |             0 |        228 |  38 |       0 |
| 3 Nov, 06:00 |  1009 |          742 |        17 |             0 |        248 |   0 |       0 |
| 3 Nov, 12:00 |  1179 |          932 |       118 |             0 |        127 |   0 |       0 |
| 3 Nov, 18:00 |  1297 |         1046 |        24 |             0 |        226 |   0 |       0 |
| 4 Nov, 00:00 |  1030 |          653 |        12 |             0 |        351 |   9 |       3 |
| 4 Nov, 06:00 |  1275 |          984 |        15 |             0 |        273 |   0 |       1 |
| 4 Nov, 12:00 |  1226 |          983 |        15 |            59 |        167 |   0 |       0 |
| 4 Nov, 18:00 |  1549 |          787 |        19 |           614 |        127 |   0 |       0 |
| 5 Nov, 00:00 |  1598 |          852 |         6 |           525 |        179 |  34 |       0 |
| 5 Nov, 06:00 |  1576 |          938 |        59 |           402 |        173 |   0 |       2 |

From 12:00 on 2 November to 12:00 on 5 November, the pass's pages from the bucket averaged 883 an hour, against the 909 it queued.

The full-resolution scans in the index on 5 November:

| Status                                                              |   Pages |
| ------------------------------------------------------------------- | ------: |
| A unit or a staging record in `usnm_quality`                        | 340,439 |
| On the staged list, with no unit or staging record                  |  15,645 |
| In `follow`'s done or skipped lists, with no unit or staging record |  61,709 |
| Neither: the 2 scans that don't decode, and pages just fetched      |      85 |
| All                                                                 | 417,878 |

LoC's pacer in CT 211 from 12:28:52 to 12:59:03 UTC on 5 November: `sent` rose by 291, 579 requests an hour, against the 1,200 an hour its normal rate allows. The fetcher wrote 182 pages in that time, 179 of them from LoC: 1.6 requests a page, counting every request through the pacer. At 12:59 the pacer's next free slot was 13 minutes in the past. It wasn't slowed, and its generation hadn't changed, so LoC hadn't pushed back.

Each figure is for that moment or window only. The fetch rate depends on which batches the z368 manifests reach, as the jump on 4 November shows.

## What is left

Each of these is the maintainer's decision:

- **Deploy `follow` with `--quarantine-decode`.** [Undecodable source scans](../boinc-quality.md#undecodable-source-scans) gives the order: the quarantine-aware manifest builder first, at an idle boundary, and the follower restarted at a safe one. #1475 has the steps. It ends the retry loop but adds no supply. The 2 quarantined scans then need a review: fetch them again or leave them out.
- **The pass's size or interval.** `MAX_PAGES` in `/etc/usnm/quality-build.env` (`--pass-pages`, 2,000) or the timer's 2 hours. The fetcher finishes a pass in about an hour of the 2 hours 12 minutes between them, so a larger or more frequent pass should raise the supply from the bucket, at the cost of more bucket requests. The bucket client's own spacing (2 seconds a request by default) caps it. How far it goes wasn't tested.
- **The order of the z368 backlog.** Taking its bucket-served batches first may raise the supply as it did from 18:00 on 4 November, with more bucket requests sooner. This wasn't tested.
- **LoC's budget.** In the half hour measured, the pacer let through under half of its 20 requests a minute and had free slots, so a larger budget wouldn't have raised the supply then. What holds the fetcher's LoC route below the budget wasn't found. The z368 manifest it was working through then had 785 of its first 828 pages already on disk.
- **The 15,645 scans on the staged list.** First confirm whether they are `v1-wave7` and `v1-wave8`'s, then load those waves' results or feed the pages again: about 10 hours of supply, once.
- **The shipper's slowdown from 12:08 on 5 November.** Find what held the step lock, and whether it lasts. While it does, more fetched pages add nothing.
- **The 61,709 scans in `follow`'s lists with no record.** Find out whether their results are only waiting to load.

What would make this note wrong: a page on both the pass's list and another manifest counts as the pass's, and a page whose source record is older than 1 November counts as LoC's.
