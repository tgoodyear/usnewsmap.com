# Japanese OCR and the OCR quality audit

Part of the [operations guide](README.md).

## Native batch-host runtime

For #919, `scripts/ocr-node/setup-ja-native.py` installs into a **new absolute
prefix** without starting a service or claiming work. It reads the NDLOCR source
commit/archive checksum, Python source set and requirements recipe directly from
`docker/Dockerfile.ja-ocr`, checks the commit against `models.lock`, and verifies
the downloaded archive and all four model files. The existing CI script-test
runner executes `test_setup_ja_native.py`; incompatible image recipe changes fail
the offline check instead of silently diverging.

On the prepared Debian batch host, with Python and its venv package installed:

```sh
python3 scripts/ocr-node/setup-ja-native.py --check
python3 scripts/ocr-node/setup-ja-native.py --prefix /opt/usnm-ja-native/REVIEWED_COMMIT
```

An incomplete installation has no `installation.json`; do not activate it. Keep
the failed prefix for diagnosis and use a new prefix for a retry. The successful
receipt records source/model/requirements hashes, installed application hashes
and Python version; the native Python version is recorded, not asserted equal to
the image's Python 3.12. Pip checks dependency consistency and all required
runtime imports before writing that receipt.

Before any production activation, run the existing headline fixture through the
installed engine as `usnm`, convert its XML with the installed `alto.py`, and
exercise the runner against **local** reference/curated stores, including a
second run that must skip completed work. Preserve the text/ALTO hashes and
resource/exit receipts. This does not publish an index.

After acceptance and coordination with the other CT215 workloads, point
`/opt/usnm-ja-native/current` at the accepted prefix and install the supplied
`scripts/ocr-node/usnm-ja-ocr-native.service`. It is manual-only, defaults to one
page per invocation, has no restart loop or enable-at-boot target, and limits
CPU to two cores and memory to 4 GiB. Put the approved `USNM_CURATED_URL` and
`USNM_REFERENCE_URL` in `/etc/usnm/ja-ocr-native.env`; an explicit `JAOCR_LIMIT`
overrides the bounded default. The `usnm` account needs the existing `himds`
group for Arc identity, not a stored credential. The page limit is soft: an issue
is processed as a unit and can exceed it. Never run both the retired
container replicas and this service. A source-rate hold remains a hold, not a
reason to restart repeatedly.

The [27 October residual-page audit](https://github.com/tgoodyear/usnewsmap.com/releases/tag/data-ja-availability-20261027)
distinguishes 19 image-listed pages from 24 explicitly undigitized placeholders.
Preserve sequence slots; image availability is not an OCR receipt. The latter
pages need legitimate alternate scans or source correction, not an exclusion
waiver. The runner retains completed image-available pages when other exact
sequence slots are explicitly marked `digitized: false`, but those undigitized
pages remain pending in the unchanged target list. Unknown missing images and
missing engine outputs still prevent the issue's available output from being
committed; neither is silently classified as undigitized.

### Opt-in native-run monitoring

For #1164, `usnm-ingest ja-ocr-run -- /absolute/runner args...` supervises exactly
one invocation without retrying it. It requires configured Application Insights,
exports `jaocr.run_heartbeat` every minute only while the child is active, and
uses the existing `ingest.command_failures` metric with command `ja-ocr-run` on
spawn/wait/nonzero-exit failure. Child stdout/stderr stays in the local journal,
not the exporter; no OCR text, command arguments, page identifiers or credentials
are metric dimensions. A cancelled supervisor kills its owned child on drop;
systemd's control-group stop/timeout remains the final bound.
The ingest CLI's standard curated/reference arguments remain required (already
present in the native unit's environment); supervision opens neither store nor
pipeline state and requires no Cosmos permission.

The monitored drop-in also opts the native runner into
`run --fail-on-issue-error`: it continues through the remaining issues without
retrying, then exits nonzero if any claimed issue failed, including incomplete
image/engine output or an unavailable item API. This detects partial as well as
all-issue failures. Explicitly undigitized pages, already-completed work and
claims held by another replica are not failures. Intentional source holds still
require stopping/disarming, never launching a retry. The runner's default exit
behavior is unchanged without this flag. Install and locally accept the reviewed
native runner supporting this flag in a new versioned prefix before activating
the drop-in; retain the prior accepted prefix for rollback.

Install the reviewed Linux `usnm-ingest` **separately** at
`/opt/usnm-ja-native/monitor/usnm-ingest`, never replacing CT215's shared ingest
binary. Write only `APPLICATIONINSIGHTS_CONNECTION_STRING` to the root-owned
`/etc/usnm/ja-ocr-monitor.env` (mode 0600), using CT215's existing environment
component and Arc Monitoring Metrics Publisher grant, as OCR pickup does.
Do not grant all on-prem hosts write access to the shared component or use a
local-auth key. Verify an actual Entra-authenticated heartbeat from
`ja-ocr-run -- /bin/true` before installing
`scripts/ocr-node/usnm-ja-ocr-native-monitor.conf` as
`/etc/systemd/system/usnm-ja-ocr-native.service.d/10-monitor.conf` and running
`systemctl daemon-reload`. The original unit/runtime is unchanged without this
drop-in. Installation does not start work or change source pacing.

`USNM_JA_OCR_ALERTS` opts into `jaocr-alerts.bicep`, independently of
`USNM_INGEST_JOBS`, with `USNM_JA_OCR_ONPREM` and the existing action group.
It reports supervised failures in the last 30 minutes. Separately,
`USNM_JA_OCR_EXPECT_ACTIVE` explicitly arms a 30-minute missing-heartbeat alert:
arm it only after the bootstrap heartbeat is visible and a run is planned;
disarm on completion, before intentional stop, idle, or a source-rate hold.
An installed runtime, old heartbeat or on-prem permission alone never implies
expected activity. With expectation armed, missing all data remains an alert,
not a success-shaped fallback when history expires. Both settings default off;
neither production settings nor alert resources are changed by the code alone.
Rules evaluate every 15 minutes and use the existing stateful action-group
pattern, about $0.50/month per enabled rule, with no new compute or ingestion key.

Before acceptance, install only the reviewed monitor/drop-in on the accepted
host, verify the existing grant and real heartbeat, exercise a controlled failed
invocation and successful recovery, and retain actual notification receipts.
Exercise expected-active stale detection and confirm disabled/intentional-idle
does not alarm. Read back only these two alert resources; avoid a broad provision
or shared-binary replacement. Retired container units are not automatically
instrumented: keep them stopped, or explicitly wrap their trusted invocation
with the same supervisor and verify that host's own existing telemetry grant.
Rollback removes only the monitor drop-in, reloads systemd and disarms the
settings; preserve the accepted OCR runtime and immutable outputs.

### Native acceptance receipt: 27 October 2026

CT215's Debian Python 3.11.2 successfully installed the pinned source/models and
requirements; all 33 runner tests passed with PyArrow, without skipped cases.
As `usnm`, the existing headline fixture produced ALTO and Parquet in 9.15 s;
a separate-process resume preserved both hashes and performed no further OCR.

The actual residual `sn83025333_1943-01-30_ed-1_seq-6` full JPEG returned HTTP
200 (1,444,637 bytes, SHA-256
`48b4668bf2a337fbb7a6b31874b90b4ffd3eb2853d0795e222c395226a73f4a3`).
Using that image, its saved official issue metadata and source ALTO geometry,
the native runner wrote one local-store page while leaving its explicitly
undigitized sequence-7 sibling pending: **one complete page, one pending page,
zero complete issues**. A separate-process resume preserved the ALTO/Parquet
hashes without repeating OCR.

The real-page lifecycle was also repeated under the proposed service's exact
`usnm`/`himds`, `NoNewPrivileges`, `PrivateTmp`, `ProtectSystem=strict`,
`ProtectHome`, state-directory and two-CPU/4-GiB controls. In that same sandbox,
native Arc authentication read the existing 12,811-target production manifest
and status without writing blobs. OCR plus local ALTO/Parquet completed in
19.71 s. These receipts prove target-host compatibility, sandboxed identity
reads and bounded native execution, not accuracy against human truth or
production publication. No source requests were made by the replay harness;
the real JPEG and geometry were fetched once beforehand at normal pace.

## Japanese OCR: mixed pages and English text

**Mixed pages** (#204, 04 §4.8). The OCR job's targets (`targets-v3`) include the Japanese titles' pages whose LoC text is from 0.35 to under 0.65 word-like (`loc_text = mixed`, about 2,000 pages). List them once on the one-replica job, then start the OCR job as usual:

```sh
RG=$(scripts/azure/settings.sh --env prod --key AZURE_RESOURCE_GROUP)
ocr/ja-ocr/tools/start-quality.sh --env prod --command targets            # caj-usnm-jaone: writes ocr-ja/targets-v3.jsonl
# its log ends with `targets written` and the pages by loc_text (mixed: about 2,000); then
az containerapp job start -n caj-usnm-jaocr-prod -g "$RG"   # `run`: OCRs the targets not done yet
```

Listing streams the archive of every batch with a Japanese title again, as the first listing did, to find the pages LoC ships without text; pages already read are skipped by the run. Another cut: `start-quality.sh --env prod --command targets --mixed-below 0.8`, then `start-quality.sh --env prod --command run --mixed-below 0.8` (its own list and claims, `targets-v3-mixed80`); 0.35 or less lists no mixed pages. At the job's pace in October 2026 (a 2-page issue in about 21 s and a 4-page one in about 38 s per replica, mostly loc.gov's pacing) the 2,000 pages take about 5 hours on the two replicas, about $4.50 of Consumption time (2 × 4 vCPU and 8 GiB at about $0.43 an hour each), plus the listing. The next release publishes their Japanese text (an overlay-only release when nothing else is new). A mixed page joins the Japanese index only with Japanese on it: expect about 200 (Rocky Shimpo's front pages).

**English text of our OCR** (#203, 04 §4.8). Off by default. With `USNM_JA_LATIN` (the job's `--ja-latin`), a release gives the pages LoC ships without text a main-index document with the Latin-script text our OCR read on them (their English ads, mastheads and sections), and gives a page LoC read badly our text in place of LoC's where ours reads more words. No full rebuild is forced:

```sh
scripts/azure/settings.sh --env prod --key USNM_JA_LATIN --value true
scripts/azure/provision.sh --env prod
```

- The next release adds the pages LoC ships without text, a few thousand documents, in a new main index: the weekly delta, or, when LoC has published no new batch, a delta of just these pages (`no newly curated batches; releasing our OCR's Latin text of pages LoC ships without text in a delta of its own`). After that, a run with no new batch and no new OCR releases nothing. The log has `indexed our Japanese OCR's Latin text` with `missing` and `in_place_of_locs`; `reference/<version>/ocr_ja.json` has `latin: {added, pages, hidden, rule}`, `ja_latin.json` lists the pages, and the build record has `features.ja_latin`.
- Our text in place of LoC's reaches a batch when a release indexes it, so for the published Japanese batches at the next [full rebuild](rebuild-and-index-storage.md#full-rebuild) (with the setting on). A delta of only the pages LoC ships without text indexes no batch, and its log line says so.
- Check: an English search for a word in a Colorado Times ad (for example `"Larimer Street"` over 1945) finds pages marked "Our OCR", whose viewer link has no highlight. The page totals don't change: these pages were counted since #139.
- Clearing the setting stops new documents; a delta keeps the ones already indexed, and the next full rebuild leaves them out.

## Check Japanese search

Our OCR of the Japanese pages LoC ships without text (04 §4.8) goes live only with a release: each release reads the OCR job's output once, near its start, and logs `Japanese OCR overlay` with the pages it took. Pages the job reads after that wait for the next release; when nothing else is new, a run releases them on their own ("releasing the new Japanese OCR on the same indexes"), or, with `USNM_JA_LATIN` and Latin text on pages LoC ships without text, in a delta of just those pages ([above](#japanese-ocr-mixed-pages-and-english-text)). After a release publishes, check from outside:

```sh
scripts/ops/check-ja-search.py --expect <issue> … --late <issue> …
```

Issues are named as the OCR job logs them (`ocr issue done`, `<lccn>_<date>_ed-<n>`): `--expect` ones the job finished before the release's `Japanese OCR overlay` line, `--late` ones after it. The script checks that `/v1/meta` names a Japanese index and its pages, that common words (日本, 戦争, 米国, 真珠湾, 収容所) return pages (a version without the index answers 422), that hits are marked as our OCR with the engine, that each `--expect` issue has pages of our OCR (the paper's hits on that day for any of の, に, は, を, in the issue's edition), and that each `--late` issue has none yet. Every request is pinned to the version `/v1/meta` named and doesn't follow redirects, so a release that publishes mid-check fails it rather than mixing versions. It sends Do Not Track, so its searches stay out of the search log, and exits 1 if a check fails.

## OCR quality audit

`jaocr.py quality` (`ocr/ja-ocr/quality.py`) measures how good LoC's OCR text is, by language and decade, to show where reading pages again would pay off, and how often a page's language differs from its title's first catalog language. It runs in the Japanese OCR job's image because the curated store is reachable only from the VNet. It reads every curated part of every batch in the published version once, keeps a fixed sample of pages (2% by default; a page is in it when a hash of its `doc_id` falls under the cut, so a rerun takes the same pages) and scores each sampled page's stored text. Word tokens are letters only (surrounding punctuation stripped, a word hyphenated across a line break joined), at least 2, case-folded.

The default metric, v2, first finds the page's language:

- Candidates are the title's catalog languages with a word list, plus English: wordfreq's, or for Hawaiian and Yiddish our own (`ocr/ja-ocr/wordlists/`, built by `ocr/ja-ocr/tools/build-wordlists.py`; sources and licenses in its README). Each language's function words are its 100 most frequent words of 2 letters or more. The winner is the language whose function words are the largest share of the page's tokens, if that share is at least 0.10, at least 10 different function words appear, and, counting only the function words that one of the two languages has and the other lacks, the winner has at least twice the runner-up's tokens. Otherwise the page is `und`: fewer than 20 word tokens, garbled, or in a language with no list (Dakota, Choctaw, Navajo, Latin, Welsh and others).
- A page whose words are mostly in another script takes the title's language in that script: Hebrew script is `yid` or `heb` (if the title lists both, the one whose function words clearly fit better), Cyrillic is Russian, Ukrainian, Bulgarian or Serbian. Serbian in Cyrillic is scored letter for letter in Latin script, against wordfreq's Serbo-Croatian list. Words are read with Yiddish ligatures (װ, ױ, ײ) as their two letters. A title that lists no language in that script gets `und`.
- `mixed`: a page of 200 words or more is cut into 50-word windows (a last, shorter window counts if it has at least 25 words; a shorter tail is left out), and each window gets a language if one clearly wins (the same test, with 5 different function words). Two languages that each win at least 20% of the decided windows, and at least 2 windows, make the page mixed. Each decided window is scored against its own language; windows no language wins (garbled, or too few function words) aren't scored. A second script holding at least 20% of the words in another of the title's languages also makes a page mixed (an English column on a Yiddish page): that part has no word minimum of its own, only the 5 different function words a window needs.

Then it scores the page against its language:

- `function_share`: the share of its word tokens that are the language's function words. Text in one language has a similar share from page to page, different for each language: on a handful of LoC pages checked while building it, about 0.41 to 0.46 for English, 0.25 to 0.31 for German, 0.36 to 0.39 for Spanish, and about 0.2 for Polish and Lithuanian, whose most common words are single letters and don't count. Compare a language's pages with its own median and read the low tail as damage.
- `damage_rate`: near-misses / (near-misses + tokens that are one of the language's 20 most frequent words). A near-miss is one OCR edit from one of those 20: a letter substituted or deleted, a thin letter (c f i j l n r t v) inserted, or one letter read as two thin ones ("h" as "li"). English "tbe", "tlie", "thc", "aud", "nnd", "ot", "iu"; German "bie", "unb", "ift", "fich", "dcr". Real words don't count: anything in the top 5,000 words of the language, of the title's other languages or of English ("she", "an", "or", "hand", "tho"). Danish and Norwegian also accept each other's words and the "aa" spelling of "å" ("paa", "aar"); Czech and Slovak each other's. A page with `damage_rate` over 0.1 counts as damaged, over 0.25 as badly damaged.
- `garbage_share`: the share of its tokens that rmgarbage-style rules flag: over 40 characters, mostly punctuation, the same letter 3 times in a row, 4 or more Latin letters with no vowel or only vowels, or a capital inside a lower-case word ("tHe").

`--metric v1` runs the first version: `dict_share`, the share of word tokens in wordfreq's top 200,000 words for the title's first catalog language. It is too lenient to rank pages: those lists hold common OCR errors ("tbe", "aud", "ift", "bie") and no frequency cut separates them from rare real words, so medians were about 0.95 for English and 0.86 for German. It also scored pages of multilingual titles against the first language listed, so Yiddish, Serbian and Polish papers catalogued as English first came out worst.

Start it as one execution of the job with `quality` and its options as the arguments; the script prints the execution's name:

```sh
ocr/ja-ocr/tools/start-quality.sh --env prod --sample-pct 10 --min-pages 50
```

`az containerapp job start --args quality` alone doesn't work: when any container flag is given, the CLI sends a container override named after the job, with no image, settings or resources. The script sends the job's own template with only the arguments changed.

The job's replicas (`USNM_JA_OCR_REPLICAS`, 2) share the work. The parts are cut into chunks of whole batches (about 40 parts each); a replica claims a chunk under `audit/ocr-quality-v2-<version>-<pct>pct.run/<execution>/claims/`, scans it, and writes its sampled pages to `pages/<chunk>.jsonl.gz` and a `done/<chunk>.json` marker. A replica renews its claims every 5 minutes; a claim not renewed for `JAOCR_AUDIT_LOCK_MINUTES` (60) is taken over, and the replica that lost it drops its copy (a chunk's pages are the same whoever scans it). So a replica that dies costs only its chunks' time. When every chunk is done, the replica that creates `reduce.lock` tallies all the chunks, writes the outputs and the final status, renewing the lock as it goes, and then writes `finished.json`. The other waits for `finished.json` (taking over a reduce lock that goes stale), logs `ocr quality results written by another replica` and exits; a retried replica of a finished run exits at once. Progress writes stop once every chunk is done, and a waiting replica restores the final status if a late one replaced it. A new execution starts a new run directory, so a rerun redoes the work and overwrites the outputs. The outputs are shared by every execution with the same version and sample size, so the reducer publishes under `audit/ocr-quality-v2-<version>-<pct>pct.publish.lock` (released after, taken over when stale): two overlapping runs publish one after the other, and the later one's files win. Each replica scores parts in 6 worker processes (`JAOCR_QUALITY_WORKERS`; reading the parts is partly I/O, so more workers than the 4 vCPUs pays) and needs well under the job's 8 GiB: each worker holds a few small word lists and a cache of the last 262,144 distinct tokens' scores. The 2% run took 69 minutes on one replica before the work was shared and the token cache added (October 2026). The job's 24-hour replica timeout is its limit. `ocr/ja-ocr/bench_quality.py` times the per-page scoring on synthetic pages.

Output, in the curated container (the data account is reachable only through its private endpoint, like the search log ([Search log](search-serving.md#search-log))):

- `audit/ocr-quality-v2-<version>-<pct>pct.json`: every table, the summary (with the language agreement shares), the word lists and thresholds used.
- `audit/ocr-quality-v2-<version>-<pct>pct-<table>.csv`, one per table:
  - `by-language-decade`, `by-language`, `by-decade`, by detected language (`mixed`, `und`, and `no_text` for empty and short pages are rows of their own): pages sampled, shares empty and short, `text_pages`, shares `und` and `mixed`, mean, median and 10th, 25th, 75th and 90th percentiles of `function_share`, mean and median `damage_rate`, shares damaged (over 0.1) and badly damaged (over 0.25), mean `garbage_share`.
  - `language-agreement`: the crosstab of the title's first catalog language against the detected page language (`mixed:ger+eng` names both), with pages and the share of the row.
  - `agreement-summary`: for all pages with text, those of single-language titles, of multilingual titles, with no title, and of each first language: the shares detected as the first language, that differ, and detected as another language, `und` and `mixed`, each of the pages with text.
  - `worst-titles` (100) and `worst-batches` (50) by median `damage_rate`, with the detected-language mix (`ger:40 eng:3 und:2`); `undetermined-titles` (100), the titles with the highest `und` share.
- `audit/ocr-quality-v2-<version>-<pct>pct-pages.csv.gz`: every sampled page but Japanese titles', one row each: `doc_id`, title, date, batch, the title's languages, the detected language and decision, the winner's and runner-up's function-word shares, the script, `function_share`, `damage_rate`, `garbage_share` and word count. About 475,000 rows at 2%, for spot checks and for deciding how to tag page languages. It is compressed as the run goes; a sample of more than 3,000,000 pages (over about 12%) gets no page file (`ocr quality pages file dropped`), only the tables.

Titles and batches enter the worst lists with at least `--min-pages` pages with a `damage_rate`, and `undetermined-titles` with that many pages with text. The same rows are in Log Analytics, one `ocr quality` line each with `metric` and `table` fields, then `ocr quality finished` with the totals; `ocr quality progress` lines come about once a minute from each replica, with the chunks and batches done so far across the run. `ocr/ja-ocr/tools/quality-rows.sh --env prod --execution <execution>` prints a run's rows as JSON lines:

```sh
scripts/azure/logs.sh --env prod --query-name ocr-quality --span 2d
```

A v2 run also writes `status/ocr-quality.json` in the reference container for the status page: `{batches: {done, total}, finished_at, metric, pages_sampled, sample_pct, started_at, summary, updated_at, version}`, at the start, with each progress line and at the end. At the end `finished_at` and `summary` are set: `summary.agreement` (`differs_share`, `mixed_share`, `und_share` of all pages with text, `multilingual_differs_share` of multilingual titles' pages) and `summary.languages`, each detected language with at least 200 sampled pages (`pages`, `function_share_median`, `damage_rate_median`, `damaged_share`). Every field is present, and null when there is nothing to measure: the agreement shares when no sampled page has text, `multilingual_differs_share` when no page of a multilingual title does (a small sample), and a language's three scores when none of its pages was scored (a language with no word list, such as Cherokee). A run that fails leaves its last progress, with no `finished_at`. A failed write is logged and the audit goes on.

Read the numbers with these limits in mind:

- It is a sample. At 2% a small title or a single decade of a rare language has few pages; check `text_pages` and `damage_pages` before trusting a row.
- `damage_rate` has a floor: real words outside the top 5,000 that are one edit from a common word ("ana", "hen", "ion" in English) count as misses, so clean English scores about 0.04 to 0.05. Old spellings do too, so compare decades within a language more than languages with each other. Misreads that are real words ("tho" for "the", "ber" for "der") and misreads two edits away ("bcr", "beu" for "der", "den") don't count, so it undercounts damage.
- `und` mixes three things: pages too short to tell, pages so garbled no language fits, and pages in languages with no word list. `garbage_share` and the `undetermined-titles` list help tell them apart. Heavily damaged pages can fall to `und` instead of their language, so a language's damage shares leave out its worst pages.
- Detection only chooses among the title's catalog languages and English: a German page in a title catalogued as English only is `und`, not German. English is always a candidate, so a page counted as English in a title that doesn't list English is English text (ads, a column) by its function words.
- Pages of titles that list Japanese are left out (our own OCR covers them) and counted in the `japanese_skipped` row.

### Damage metric parity (#296)

The v2 metric's per-page code is `ocr/ja-ocr/damage_v2.py`, which `quality.py` imports. `usnm_core::damage` (cargo feature `damage`) is its Rust port, which the OCR pipeline uses (12 §12.5 Steps 5 and 6). Two checks keep them the same: the same language, decision, second language and script, equal token, word, head, near-miss and function-word counts, and rates within 1e-9.

- In CI, `cargo test -p usnm-core --features damage` scores the pages of `crates/usnm-core/tests/data/damage-parity.jsonl.zst` again: 66 pages of loc.gov's OCR text from 33 titles in 25 catalog languages, and 17 made-up pages for the traps between Python's string handling and Rust's. It fails when `damage_v2.py`'s sha256 isn't the one the file records. After a change to `damage_v2.py`, port the change and score the same pages again: `python3 ocr/ja-ocr/damage_parity.py --rescore <old file> --out <new file>` (wordfreq 3.1.1; no loc.gov requests). `--pages-from <file> --out <path> --synthetic` builds a new one from loc.gov, with jaocr.py's pacing.
- In the cloud, on curated text: `jaocr.py damage-parity` samples 400 pages of the 10% audit's per-page CSV, spread across languages, decisions and scripts, and writes `audit/damage-parity/<stamp>.jsonl.zst` in the curated container. It logs each page's doc_id and title languages (`damage parity page`), never text. `usnm-ingest ocr-damage --parity <that path>` scores them in Rust ([below](#ocr-damage-tables-296)).

```sh
ocr/ja-ocr/tools/start-quality.sh --env prod --command damage-parity --pages 400
```

### OCR damage tables (#296)

`usnm-ingest ocr-damage` scores pages with the Rust port (12 §12.5 Step 6). It runs on-prem in pve-laptop CT 215 (`usnm-batch`), as that container's own Arc machine identity (#917): run as root on pve-laptop, `usnm-ingest-run` starts one command as a transient systemd unit with the container's settings and its installed release (home-infra `docs/lab.md`, "Ingest commands"); its log is `journalctl -u usnm-ingest-ocr-damage-<stamp>` in the container. The grants are in "On-prem" below. It logs counts and doc_ids, never text. Run the three modes in this order: parity, the gate, then the full pass.

1. **Parity on curated text** (Step 5's cloud check). Write the pages with `damage-parity` ([above](#damage-metric-parity-296)), take the path from its `damage parity written` line, then:

   ```sh
   pct exec 215 -- /usr/local/bin/usnm-ingest-run --command ocr-damage -- --parity audit/damage-parity/<stamp>.jsonl.zst
   ```

   It logs `ocr damage parity` with the pages that agree and up to 50 doc_ids that don't, and fails if any page differs.

2. **The gate.** The 10% audit's pages, picked by the audit's own sampler from every curated row of `pages-v20261003-1` (duplicate copies included), scored in Rust and compared with the audit's per-page CSV, joined on doc_id and batch. The CSV rounds every score to 4 decimals, so Rust's are formatted the same way before the comparison. A page agrees when the language, the decision and the three scores all match; the gate passes when at least 99.99% of the CSV's 2,368,911 rows agree. It writes no tables, only its result, `audit/damage/v2/gate/<version>-<pct>pct-<stamp>.json`, and the `ocr damage gate` line (counts by kind of difference, up to 50 doc_ids). It fails when the gate does. The result goes in a draft report PR.

   ```sh
   pct exec 215 -- /usr/local/bin/usnm-ingest-run --command ocr-damage -- --version pages-v20261003-1 \
     --sample-pct 10 --compare-csv audit/ocr-quality-v2-pages-v20261003-1-10pct-pages.csv.gz
   ```

3. **The full pass**, on the live version (`index_version` in the reference container's `current.json`), once the gate has passed:

   ```sh
   pct exec 215 -- /usr/local/bin/usnm-ingest-run --command ocr-damage -- --version <live version> --max-runtime-secs 165600
   ```

   Each batch version gets `audit/damage/v2/<batch>_ver<NN>.parquet`, one row per page: `doc_id`, `lccn`, `date`, `edition`, `seq`, the base `batch`, `batch_version`, `text_status`, `lang`, `decision` (`detected`, `mixed`, `und`, or `no_text` for pages without text), `mixed_with`, `script`, `damage_rate`, `function_share`, `garbage_share`, `words`, `head` and `near`. Beside it, `<batch>_ver<NN>.summary.json` holds per-title counts (pages, scored pages, und, mixed, damaged and badly damaged), the median damage rate and pages by language. The summary is written last and marks the batch version done, so a later run skips it and pays only for new batches. When every batch of the version has its tables, `audit/damage/v2/versions/<version>.json` lists them; the seeder and `ocr-sync` read that list. A second run on the same version writes nothing. `--batches a,b_ver02` scores only those batches and doesn't write the list. A page is scored against its title's languages in the version's `titles.json`, which the command requires (the working catalog only fills in titles it lacks). A later correction to a title's catalog languages doesn't rescore tables already written. Tables are create-only, so to rescore, delete both the `.parquet` and the `.summary.json` of each of that title's batch versions, then run the pass again.

   `--max-runtime-secs` stops the run from starting new batches (it finishes those it has started), and the next run continues. `ocr damage progress` lines come once a minute with the pages per second. Batches are scored `--parallel` at a time, by default one per CPU. On one laptop core the scoring runs at about 1,650 pages a second, so the corpus's 23.7 million pages are a few hours of CPU on the job's 4 vCPUs, plus reading the curated parts. The Python audit's 10% sample took 5,725 s. Time the first full run and record it here. On the Consumption profile the job costs about $0.43 an hour, and reading curated parts that have moved to Cool adds about $0.01 per GB read.

**On-prem.** Since 24 October 2026 the pass can run on an on-prem host instead, as that host's own Arc machine identity (pve-laptop CT 215, `usnm-batch`). With `USNM_OCR_DAMAGE_ONPREM` on and `USNM_OCR_DAMAGE_RUNNER` set to that identity's object id, provision grants it a role that can only create new blobs, under `curated/audit/damage/`, with no read, overwrite or delete (`infra/modules/damage-writer.bicep`). It reads curated and reference through the on-prem hosts' group (`USNM_JA_OCR_ONPREM`). Run it detached at nice 19, as above:

```sh
pct exec 215 -- /usr/local/bin/usnm-ingest-run --command ocr-damage --nice 19 -- --version <live version>
```

**A dev-scale test** (dev holds nothing outside a focused experiment, so this needs the maintainer's approval first). Locally, from the repository root:

```sh
python3 scripts/dev/fixture-batches.py --out-dir /tmp/usnm-dmg
cd /tmp/usnm-dmg
ingest="cargo run --release --manifest-path $OLDPWD/Cargo.toml -p usnm-ingest --"
$ingest --state-file state.json --curated curated --reference reference run --list batches.json --synthetic --index-dir reference/indexes
v=$(jq -r .index_version reference/current.json)
USNM_REFERENCE_URL=$PWD/reference USNM_CURATED_URL=$PWD/curated python3 $OLDPWD/ocr/ja-ocr/jaocr.py quality --sample-pct 100
$ingest --state-file state.json --curated curated --reference reference ocr-damage --version "$v" \
  --sample-pct 100 --compare-csv "audit/ocr-quality-v2-$v-100pct-pages.csv.gz"
$ingest --state-file state.json --curated curated --reference reference ocr-damage --version "$v"
$ingest --state-file state.json --curated curated --reference reference ocr-damage --version "$v"   # writes nothing
```

On 10 October 2026 that gate agreed on all 1,872 fixture pages, and the second full run skipped both batches. In dev, after `scripts/azure/provision.sh --env dev`: curate and release a few small batches (`dlc_zurich_ver04,dlc_misctopsn83025894_ver01,dlc_misctopsn83021129_ver01`), run `ocr/ja-ocr/tools/start-quality.sh --env dev --sample-pct 100`, then the three commands above with `--env dev`, the dev version and the dev audit's CSV, and `--command damage-parity --pages 50`. Check the tables, the gate result and the second run's `skipped`, then `scripts/azure/teardown.sh --env dev`.

## Text the Japanese titles' pages hold that search can't reach

`jaocr.py mixed` (`ocr/ja-ocr/mixed.py`) measures two gaps the Japanese OCR leaves. The OCR job read a page only when LoC's text for it was missing, empty, short or under 35% word-like; a Japanese query searches only our OCR, any other query only LoC's text. Its October 2026 results led to the `mixed` targets and the English text of our OCR in the main index ([above](#japanese-ocr-mixed-pages-and-english-text)); the command still measures by the 35% test.

- Japanese on pages LoC read as words: a page with an English column and a Japanese one, read in English, can pass the 35% test, and its Japanese column is then garbled Latin. For every page with LoC text of a title that lists Japanese, in the batches that hold such titles, it counts pages by word-like share (bands from under 0.35 to 0.9 and over); the other titles in those batches, same scanning and OCR but English only, are the control.
- English on the pages we read: NDLOCR-Lite reads some English (mastheads, short lines), but only into the Japanese index. For every page of our OCR it counts the tokens that are among English's 5,000 most frequent words (3 letters or more).

```sh
ocr/ja-ocr/tools/start-quality.sh --env prod --command mixed          # prints the execution's name
ocr/ja-ocr/tools/quality-rows.sh --env prod --execution <execution>   # its "mixed pages" rows, once it has finished
```

It writes `audit/mixed-pages-<version>-<execution>.json` and a CSV per table in the curated container (named by execution, so two runs never write the same files): `loc-text-bands` (Japanese titles against the control), `loc-text-by-title`, `loc-text-samples` (15 page ids per band, with their loc.gov links, for looking at the scans), `our-ocr-english` (by why LoC's text wasn't used), `our-ocr-by-title` and `our-ocr-samples`. It runs on the one-replica audit job, `caj-usnm-jaone-<env>`, which `start-quality.sh` picks for it. Should a second replica run it anyway (the ja-ocr job), one does the work, renewing a lock; the other waits for its finished marker (`mixed pages written by another replica`) and takes over if the lock goes stale, and a retried replica of a finished execution exits at once. `our-ocr-english` and `our-ocr-by-title` also give the median Latin share of our OCR's letters and the share of pages over half Latin.

## The Japanese OCR on-prem

The Japanese OCR (`ocr/ja-ocr/jaocr.py run`, #128) runs on an OCR node instead of the Azure job `caj-usnm-jaocr-<env>` (#917, which moves work out of Azure; serving moves on-prem too, under #955 and [ADR-0027](../design/adr/0027-onprem-serving-behind-cloudflare.md)). It's the same image, built on the node from a pinned commit (`docker/Dockerfile.ja-ocr`), as the node's Arc machine identity.

1. **Settings.** `USNM_JA_OCR_ONPREM=true` and `USNM_JA_OCR_JOB=false` in `infra/env/<env>.json` (through a pull request), with `USNM_ONPREM_GROUP` set and the node's Arc identity in that group. Provision. That grants the group read on `curated` and `reference` and create-only writes under `curated/ocr-ja/` and to `reference/status/ocr-ja.json`, with no delete (`infra/modules/jaocr-runner.bicep`). It opens the lake to `USNM_OCR_NODE_IPS` and removes the Azure job.
2. **Install and start**, on the node as root, from a checkout of the scripts:

   ```sh
   scripts/ocr-node/ja-ocr-onprem.sh --commit <sha on main> \
     --curated-url https://<lake>.blob.core.windows.net/curated \
     --reference-url https://<lake>.blob.core.windows.net/reference --replicas 2 --start
   journalctl -fu 'usnm-ja-ocr@*'
   ```

   Each replica (`usnm-ja-ocr@N`) gets 4 CPUs and 8 GiB, as in Azure. The script multiplies loc.gov's gaps by `JAOCR_REPLICAS`, so the replicas stay under LoC's limits together. Don't run them while another host is fetching from LoC at full pace.

3. **Until done.** A run ends when no target issue is left to claim; like the Azure job, start it again (`systemctl start usnm-ja-ocr@1 usnm-ja-ocr@2`) until the status page shows every target done. A newly added host needs a fresh Arc token before its group grants apply: restart `himdsd`.

The one-off measurements (`mixed`, `american-stories`, `quality`), which ran on `caj-usnm-jaone-<env>`, aren't covered by this grant: it only lets the runner write under `ocr-ja/`.
