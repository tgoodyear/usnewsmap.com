# Serbian pages read by two engines and held from publishing

**Status:** live since 7 November 2026 · **Date:** November 2026 · **Policy:** [`2026-11-07-serbian-trust-hold.json`](2026-11-07-serbian-trust-hold.json) · **Code:** `boinc/scripts/batch_scheduler.py` (`--claim-language`), `boinc/scripts/onprem/quality-feed/qwen_feed.py` (`--serbian-budget`, `--serbian-pages`), `crates/usnm-boinc/src/trust/tests.rs`; [German hold](2026-11-05-german-trust-hold.md), [gold check](../../boinc/docs/gold-check.md#host-trust-for-usnm_ocr), [the claim](../boinc-batch-scheduler.md#the-claim) · **Issues:** #835

## The question

The corpus has 43,258 Serbian (`srp`) pages, all from one title, LCCN sn85055101 (*Amerikanski Srbobran*, 1906–1963). None of them had a reading of ours: no Tesseract page OCR and no Qwen crop reading. LoC's text for them is Cyrillic. To compare LoC's reading with ours on check.usnewsmap.com, the pages need our readings, but nothing calibrates Serbian OCR yet, so those readings mustn't reach search.

The maintainer decided on 7 November: hold Serbian the way German is held, add `srp` to the BOINC claim, queue the pages for Tesseract, and give the Qwen feed a capped Serbian budget.

## What we did

- **A Serbian hold in `trust.json`.** The reviewed list is the live German hold list with one more held scope: `srp`, profile `srp+srp_latn`, PSM 3, 300 DPI, no hosts. `collect --trust` keeps those results in `quarantine/<job>/`, marks the job `held` and publishes nothing; English and German are unchanged.
- **The scope is the one a Serbian job carries.** `export` sets PSM 3 and 300 DPI on every job, and takes the profile from `engine::route`: the first catalog profile for the language whose models the image's `models.lock` holds. For `srp` that is `srp+srp_latn` (`ocr/engines/engines.toml`). The test calls the same route, so a catalog change that moves Serbian to another profile fails it.
- **`max_cer` 0.179 is a placeholder.** There is no Serbian gold set, so no measured limit. It is German's figure, and a calibration that lifts the hold has to come in at or below it. Relaxing it is a one-line reviewed edit.
- **The manifest digest is the primary scope's** (`d8cd7279…`, the English challenge). German kept its earlier entry's digest because there is no German challenge yet. Serbian had no earlier entry, and a held scope's digest isn't checked at collect, so it names the challenge the trusted hosts were last judged on. The calibration that lifts the hold brings its own.
- **`--claim-language srp` on the controller.** `claim_languages` is part of the controller's allocation digest, so editing it would stop the controller with "immutable allocation changed" until a drain and a new plan. The flag widens the list for the claim step only, keeps the digest, and is in the plan event.
- **Qwen: `--serbian-budget 500 --serbian-pages FILE`.** Modelled on the German budget. The 500 pages are the first 500 of the Tesseract order below, so both engines read them. Serbian chunks go to the version-3 root. The refill already admits Serbian, so its env and pin didn't change. 500 is the total cap: raising it is an env change.

### The pages

Pages are queued in a fixed order that takes them round-robin across decades (within a decade by `md5(doc_id)`), so each batch covers 1906–1963 evenly. The first batch was 5,000 pages: 585 from the 1900s (all of them) and 735–736 from each later decade. The Qwen list is the first 500 of the same order.

## Rollout, 7 November (ET)

| Time | Step | Evidence |
| --- | --- | --- |
| before | Checked the hold code is live | Server image `sha256:a877ccf1…` (source `94b19d0b`, after #1574) has the hold string; its trust and routing code equals main's. CT 211 was at `8bdf1da1`, after #1574. |
| 1:59 PM | Swapped `gold/trust.json` under the gold lock | `53bc508e…` → `442270d7a84112e2a9aed68b7edbd42e33b3b58b4461c8e5ce89a18cb5c496c5` (1,783 bytes). Backup `trust.json.pre-serbian-20261107`. |
| 2:00 PM | Next tick | 17 English pages published, `collection_backlog` 0. |
| 2:05 PM | Moved CT 211's checkout to this branch and added the flag | Owned-record check on a state copy: 1,148 journals, same under both versions; dry run digest `163f3d51…` unchanged, `claim_languages` `["eng","deu","srp"]`. Backup `/etc/usnm/batch.conf.pre-serbian-20261107`. |
| 2:09 PM | One tick by hand | 13 English pages published. Timer restarted. |
| 2:11 PM | Queued 5,000 pages, plan `serbian-compare-20261107` | Dry run: 5,000 would queue, no skips. Ledger `ocr-local/queued/2026-11-07/20261107T190955815445Z-3f44bb74.parquet`. |
| 2:12 PM | Qwen feed Serbian budget | Feed backup `qwen_feed.py.pre-serbian-20261107`, env backup `qwen-feed.env.pre-serbian-20261107`. |

## Rollback

- **List:** restore `gold/trust.json.pre-serbian-20261107` under the gold lock, but only once no Serbian job is in flight. Under a list without the scope a Serbian result is refused, not held, and a refused result stops the refill. Drop `--claim-language srp` first and let the claimed jobs settle.
- **Controller:** restore `batch.conf.pre-serbian-20261107`, `systemctl daemon-reload`, and check out `8bdf1da1` again, between ticks. Serbian jobs already claimed are still collected under the list.
- **Qwen feed:** restore `qwen-feed.env.pre-serbian-20261107` and `qwen_feed.py.pre-serbian-20261107`. The pages in `serbian-budget.json` that already shipped stay in their root.

## What is left

- **Queue the other 38,258 pages** in batches of up to 10,000 from the same order, after the first batch's held count rises as expected. Scans arrive through the normal fetcher, at most 1,000 an hour.
- **The held backlog.** Held journals count toward the `MAX_JOBS` caps, as German's did. Archive them with `archive_history.py` (#1483, #1606) as for German.
- **Nodes.** `trust.json` governs BOINC results only. A held page's queue message returns when its 36-hour lease ends, and an on-prem OCR node would publish it. No node reads the queues now (OCR runs only through BOINC); that has to stay so while Serbian is held.
- **Calibration** needs a Serbian gold set (Cyrillic and Latin script), host reports for the image, and `gold trust` at or below 0.179.
