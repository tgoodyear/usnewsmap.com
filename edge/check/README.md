# check.usnewsmap.com

A public page where anyone can help check OCR text ([#1620](https://github.com/tgoodyear/usnewsmap.com/issues/1620)). Two readings of a printed word disagree, for example LoC's OCR and an aligned reference text. The page shows the printed word and both readings, and asks which one matches the print, or neither, or "can't tell", or "not text" when the box holds no letters (a picture or a smudge). Readers don't need to know the language: they compare letter shapes.

Cherokee (the _Cherokee Phoenix_, 1828–34) was the first language, Yiddish (New York dailies, 1910–53) the second and Serbian (_Amerikanski Srbobran_, Pittsburgh, 1906–63) the third. The data model assumes none of them: a language is a configuration plus a set of tasks.

## Pieces

| Piece                         | What it is                                                                            |
| ----------------------------- | ------------------------------------------------------------------------------------- |
| `worker.mjs`                  | The Worker: `/api/*` for the page, `/clips/*` from R2, `/admin/*` for loading data.   |
| `public/`                     | The page (static assets). `/` lists languages; `/<code>` (e.g. `/chr`) shows words.   |
| D1 `usnm-check`               | `languages`, `tasks`, `answers` (`schema.sql`).                                       |
| R2 `usnm-check-clips`         | Line and word images, and the example letters, at `<lang>/<set>/...`.                 |
| Turnstile widget `usnm-check` | The browser check before a session is issued.                                         |
| `tools/build_tasks.py`        | Turns line images with two readings per line into tasks, clips and a configuration.   |
| `tools/yiddish_lines.py`      | Makes `build_tasks.py`'s input for Yiddish: LoC's ALTO against our `yidj2` model.     |
| `tools/serbian_lines.py`      | Makes `build_tasks.py`'s input for Serbian: LoC's ALTO against our held BOINC ALTO.   |
| `tools/from_tap_data.py`      | Turns a glyph-check data file (Cherokee) into tasks, clips and a configuration.       |
| `tools/phoenix_lines.py`      | Picks Cherokee lines from LoC's ALTO and our production re-read for `build_tasks.py`. |
| `languages/<code>.json`       | A language's name, intro, writing direction and letter notes, plus build options.     |
| `load.sh`, `set-secrets.sh`   | Loading data and setting secrets, run by hand.                                        |

## Who answers, and how answers count

No account is needed. The first answer from a browser runs a Turnstile check, and the Worker issues a signed session holding a random id (30 days, `localStorage`). Answers are stored against that id, so one browser's answer to a word counts once. A later answer replaces it. Nothing else about the person is kept. One id may send at most 30 answers in 10 seconds.

A word **settles** once at least 3 answers are in and one choice has two thirds of them. That choice becomes its outcome: `a`, `b`, `neither`, `unsure` or `notext`; a tie goes to the first in that order. After 7 answers without that agreement, it closes as `disputed`. Settled words leave the queue. `/api/next` gives a person a word they haven't answered, choosing words nearest to settling first, so checked words come in steadily.

Seed answers (from `load.sh --seed`) are stored as contributor `seed:<name>` and count as one person.

Answers are published under CC0. `GET /api/export?lang=chr` returns one JSON line per word, with its readings, which is which (`a_is`, `b_is`), the count of each answer and the outcome. Contributor ids are never exported.

## API

| Method and path                           | Who         | What                                                                                                       |
| ----------------------------------------- | ----------- | ---------------------------------------------------------------------------------------------------------- |
| `GET /api/config`                         | anyone      | The Turnstile site key.                                                                                    |
| `GET /api/languages`                      | anyone      | Languages with task and settled counts.                                                                    |
| `GET /api/languages/<code>`               | anyone      | A language's configuration.                                                                                |
| `POST /api/session`                       | anyone      | `{turnstile}` → `{session}`.                                                                               |
| `GET /api/next?lang=<code>[&skip=id,...]` | session     | `{task, answered}`; `task` is null when none is left for this person.                                      |
| `POST /api/answers`                       | session     | `{task, choice}`: `a`, `b`, `neither`, `unsure` or `notext`.                                               |
| `GET /api/stats?lang=<code>`              | anyone      | Tasks, settled, answers, people.                                                                           |
| `GET /api/export?lang=<code>`             | anyone      | CC0 JSON lines (above).                                                                                    |
| `PUT /admin/clips/<key>`                  | admin token | An image (JPEG, PNG or WebP).                                                                              |
| `PUT /admin/languages/<code>`             | admin token | A language's configuration. With `?keep=examples` (an existing language only), the stored `examples` stay. |
| `POST /admin/tasks`                       | admin token | JSON lines of tasks; a task already loaded keeps its answers.                                              |
| `POST /admin/answers`                     | admin token | JSON lines of seed answers, `seed:` contributors only.                                                     |

A task, as loaded: `id` (`<lang>/<set>/<word id>`), `lang`, `set`, then what the page shows: `page`, `source` (the page on Chronicling America), `label`, `line` and `word` (clip keys), `box` (the word in the line image, as fractions `[x, y, w, h]`), `a`, `b`, `a_diff`, `b_diff` (indexes of the letters that differ) and `a_is`, `b_is` (what each reading is, e.g. `reference` and `loc`). The page shows `a` and `b` in an order fixed by the task id, so neither side is always first.

## A language's configuration

`languages/<code>.json` is served at `/api/languages/<code>`, except for the build options, which the build tools remove:

| Field                           | What it is                                                                                                                                           |
| ------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------- |
| `name`, `title`, `intro`        | Shown on the page. `intro` covers the source and anything specific to the script; the answer rules every language shares are in `public/index.html`. |
| `dir`                           | `rtl` for right-to-left scripts (Hebrew letters); the readings are then set right to left.                                                           |
| `notes`                         | A short note per letter, shown under a reading when that letter is highlighted.                                                                      |
| `drop_examples`, `min_examples` | Build options for `from_tap_data.py`'s example letter pictures (not served).                                                                         |
| `equivalents`                   | Letters that print alike (`"װ": "וו"`): `build_tasks.py` compares them as equal, and the page sets the two-letter spelling with no space inside it.  |

The page highlights letters as `a_diff` and `b_diff` count them: a base character with the marks that follow it (פּ, אַ) is one letter.

## Making tasks from two readings

`tools/build_tasks.py` takes JSON lines, one printed line each: its image, reading a with each word's box in the line image (as ALTO or hOCR gives them), and reading b's text. It pairs b's words with a's by aligning them, and writes a task for each paired word that differs, with a word clip cut from the line. A word one reading split, joined or garbled (less alike than `--min-similarity`) is left out, and so is a pair with no letter or digit. Its docstring has the input format. Tests: `python3 -m unittest discover -s edge/check/tools` (the clip test needs Pillow).

Yiddish uses the pages drawn for the newspaper line set ([`ocr/bench/yiddish-newspaper/`](../../ocr/bench/yiddish-newspaper/README.md)). `tools/yiddish_lines.py` fetches each page's ALTO and scan from loc.gov, takes the lines at the page's anchors as the set's protocol does (three blocks of three lines), leaves out lines LoC's OCR didn't read as text (photos, halftones and smudges, by `text_like`), and reads each line with `yidj2` ([`ocr/models/yidj2/`](../../ocr/models/yidj2/README.md)). Reading a is LoC's OCR and reading b is `yidj2`'s. `tools/Containerfile` has Tesseract and Pillow. On the build host:

```sh
podman build -t usnm-check-tools tools
podman run --rm -v "$PWD:/work" usnm-check-tools python3 tools/yiddish_lines.py \
  --manifest manifest.json --work /work/yi --tessdata /work/tessdata --model yidj2
podman run --rm -v "$PWD:/work" usnm-check-tools python3 tools/build_tasks.py --lines yi/lines.jsonl \
  --images yi --lang yi --set loc-yidj2-v1 --a-is loc --b-is yidj2 \
  --language-config languages/yi.json --out-dir yi/check-out
```

`tessdata/` holds `yidj2.traineddata` from its release (check its sha256 against `ocr/models/yidj2/README.md`).

Serbian uses pages BOINC OCR'd and held from publication (#835). The hand-off keeps each held job's ALTO at `dispatch/quarantine/<job>/layout.xml`. Fetch those for a sample of held pages into one folder (`<doc id>.xml`), and list the pages in a JSON-lines file with LoC's ALTO URL and, when the scan is on the build host, its path under `/tank/ocr/scans/keep` (the format is in `tools/serbian_lines.py`'s docstring). The script picks Cyrillic lines from LoC's ALTO and takes our words inside each line's box. `--skip-punctuation-only` leaves out words whose readings differ only in punctuation. On the build host:

```sh
podman run --rm -v "$PWD:/work" -v /tank/ocr/scans/keep:/scans:ro usnm-check-tools python3 tools/serbian_lines.py \
  --pages /work/pages.jsonl --ours /work/ours --work /work/sr --scans /scans --lines-per-page 4
podman run --rm -v "$PWD:/work" usnm-check-tools python3 tools/build_tasks.py --lines sr/lines.jsonl \
  --images sr --lang sr --set loc-boinc-tess-v1 --a-is loc --b-is tesseract \
  --language-config languages/sr.json --out-dir sr/check-out --skip-punctuation-only
```

The first set (8 November 2026) is 140 pages, 20 per decade, up to 4 lines each: 488 lines and 939 words.

## The Phoenix re-read sets (#1620, #1621)

The 190 _Phoenix_ pages that `chr_phoenix` re-read in production have two readings of every Cherokee line: LoC's and ours. Two more Cherokee languages hold tasks made from them. Each has its own queue and page, and the first Cherokee set (`chr`, its tasks and answers) is untouched.

| Language, page             | Set           | Tasks | What                                                                                                                                                                               |
| -------------------------- | ------------- | ----: | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `chr-lines` (`/chr-lines`) | `disputes-v1` |   680 | The one word where the two readings differ, on lines that differ in that word only. The 29 pages of test set v2 only: 312 news lines and 368 lines that reprint the New Testament. |
| `chr-rare` (`/chr-rare`)   | `rare-v1`     |   247 | Words with a letter the glyph bank has few or no samples of, on pages outside the test set. Tasks per letter: Ꮊ 15, Ꮌ 32, Ꮚ 40, Ꮛ 40, Ꮬ 40, Ꮯ 39, Ꮾ 41. Ꮍ has no candidate word.   |
| `chr-rare`                 | `nah-v1`      |   120 | A word's first letter: the New Testament spelling with Ꮎ against the same word with Ꮐ, on pages outside the test set.                                                              |

**Disputes.** `phoenix_lines.py disputes` pairs each line of LoC's ALTO with the re-read's line at the same position (the re-read numbers its lines afresh, but keeps LoC's line boxes) and keeps lines where both readings have the same number of syllabary words, at least three, and exactly one word differs. A pair is left out when both words are one or two letters with none in common, when they are less than half alike, or when they differ only in a word-initial G-shaped letter (that is the Ꮐ question below). A line with one disputed word is fully resolved when that word settles on one of the two readings, which takes three answers at best. A line with two disputed words needs six. About a third of the first hand check's disputed words ended in "neither", which leaves the line open, so only one-word lines are loaded. News lines are the ones the test set did not align to the New Testament. Lines with more than one differing word are not loaded.

**Rare letters.** `phoenix_lines.py rare` takes words from the 161 pages outside the test set, since the glyph bank never takes a test-set page. A word is a candidate in one of three cases. In `ours-loc`, the readings differ and one of them has the letter. In `agree`, both readings have the letter, and the second reading swaps it for a letter the readings trade it with elsewhere (a swap that gives a New Testament form is preferred). In `word-list`, LoC's word is not a New Testament form, OCR reads it rarely, and putting the letter in makes a New Testament form. Candidates are dealt round-robin over pages, at most two a page and 40 a letter. `phoenix_lines.py nah` takes words whose first letter OCR reads as Ꮎ or one of the G-shaped letters Ꮹ, Ꮳ, Ᏻ, Ꮐ, where the New Testament spells the word with Ꮎ and not Ꮐ (ᎾᏍᎩ read ᏩᏍᎩ): reading a has Ꮎ, reading b has Ꮐ. The language's notes describe the type specimens' shapes in words; the specimen plates themselves are not reproduced.

```sh
D=/scratch/usnm-chr/check-tasks   # on the build host: prod-alto/ holds the 190 re-read ALTO files
for m in disputes rare nah; do
  podman run --rm --user $(id -u usnm):$(id -g usnm) -v /scratch/usnm-chr:/chr:ro -v $D:/work usnm-check-tools python3 -I /work/phoenix_lines.py $m \
    --prod-alto /work/prod-alto --loc-alto /chr/bank/loc --scans /chr/bank/images --pages /work/pages.jsonl \
    --test-lines /chr/testset/crops/lines.jsonl --wordlist /work/chr-attested.txt --out-dir /work/out
done   # rare takes --per-letter 40 --per-page 2, nah --per-letter 120 --per-page 3
python3 tools/build_tasks.py --lines out/disputes/lines.jsonl --images out/disputes --lang chr-lines --set disputes-v1 --a-is loc --b-is chr_phoenix \
  --language-config languages/chr-lines.json --examples examples.json --out-dir out/chr-lines-disputes-v1
```

`examples.json` holds the letter pictures `/api/languages/chr` already serves (the `chr-rare` copy keeps one of Ꮎ's three, the θ-shaped one, so the pictures don't say what the first letter should look like). Load each set from the operator's machine as [below](#loading-a-language), with `--lang chr-lines` or `--lang chr-rare`. `phoenix_lines.py` also writes `manifest.jsonl` (every word of both readings of each loaded line) and, for `rare` and `nah`, `glyph-index.jsonl` (each word's box on the scan); the report's directory keeps copies as `check-lines-manifest.jsonl` and `check-glyph-index.jsonl`.

**Scoring.** `docs/reports/2026-11-cherokee/scripts/chr_disputes_score.py` recomputes the held-out score as answers arrive:

```sh
curl -s 'https://check.usnewsmap.com/api/export?lang=chr-lines' > check-chr-lines.jsonl
python3 docs/reports/2026-11-cherokee/scripts/chr_disputes_score.py --export check-chr-lines.jsonl \
  --manifest docs/reports/2026-11-cherokee/check-lines-manifest.jsonl --target 300
```

Its `progress` block counts lines fully resolved against the target of 300, the lines that settled without a winner or are still waiting, answers per resolved line, and how many resolved lines the loaded ones are expected to give. `score` gives each reading's letter error against the checked text and their difference with page-bootstrap intervals; the checked text is LoC's line with the disputed word replaced by the settled reading, so errors both readings share are not seen. Lines that settled on "neither" are left out of the main figures. `equal_error_assumption` adds them with one letter wrong for each reading, which assumes those lines change nothing, and `worst_case` gives the bound on net letter errors: a "neither" word moves them by at most the distance between the two readings, whatever the print says.

**Glyph bank.** `usnm-glyphs add-checked` turns the settled `chr-rare` words into labelled glyphs for the next training round. It cuts each word that checkers settled on reading `a` or `b` from its scan, splits it into one glyph per letter of that reading, and writes a new bank beside the old one. It never takes a glyph from a test-set page (`--exclude-manifest`). Rows from it carry `how: "checked"`, and `clean` keeps them whatever their neighbours say.

```sh
usnm-glyphs add-checked --bank bank --out bank-checked --export check-chr-rare.jsonl \
  --index check-glyph-index.jsonl --images images --exclude-manifest testset/v2/manifest.json
```

## Blind picks: Xiaomi against LoC on English (#1867)

`en` holds a blind comparison of Xiaomi-OCR-0 and LoC's OCR on Chronicling America English pages. It uses the same pick page and answers as the other languages; nothing is new on the page. Nothing is loaded yet, so `/en` doesn't exist and the language list doesn't show it: `languages/en.json` is skipped by the language sync until the language is loaded.

**Blind tasks.** A task body may carry a `private` object. The Worker leaves it out of `/api/next`, and out of `/api/export` until the task has settled; before that the export has the readings, counts and `strata` but not `a_is`, `b_is` or `truth`. A task with no `private` behaves as before. `en_pick.py` puts in `private`: `a_is` and `b_is` (`xiaomi` or `loc`; which reading is `a` is drawn from the seed and the task id), `strata` (`decade`, `damage_band`), `crop`, and for a calibration item `truth` (`xiaomi`, `loc` or `neither`: which reading matches the human transcription exactly), `truth_word` and `closer`.

**Making tasks.** `tools/en_pick.py` takes one JSON line per page (LoC's ALTO, the page image, the damage rate, and each crop's box and Xiaomi text; its docstring has the format). It takes LoC's words in each crop, aligns them with Xiaomi's words and keeps the words where they differ, but only when the alignment is certain: the readings agree on most of the crop, the differing stretch has agreeing words on both sides and as many words on each side, and each pair is alike enough. Words it cannot place (at a crop's edge, split, joined or dropped, or too unlike) are counted in `report.json`, never guessed. It then draws up to `--per-page` words per page (one per distinct pair of readings) and at most `--per-stratum` per decade and damage band, with `--seed`, and cuts the word and line images from the page image at LoC's boxes.

**Calibration.** There is no gold-item machinery: the Cherokee sets' seed answers count as one person's answer. A calibration set is ordinary tasks, in the same language and queue so volunteers can't tell them apart, whose `private.truth` comes from a human transcription (en-op, en-as). `tools/en_pick_gold.py` makes the pages file from a gold set directory (crops, manifest, ALTO, Xiaomi readings). Load them as a separate `--set` and never seed answers for them.

```sh
python3 tools/en_pick_gold.py --set-dir sets/en-op --xiaomi xiaomi.jsonl --id-prefix en-op/ --alto-dir loc --out pages-cal.jsonl
python3 tools/en_pick.py --pages pages-cal.jsonl --set cal-v1 --language-config languages/en.json --out-dir out-cal --per-page 20
python3 tools/en_pick.py --pages pages-pilot.jsonl --set xiaomi-loc-v1 --language-config languages/en.json --out-dir out-pilot --per-page 8 --per-stratum 500
```

**Analysis.** `tools/en_pick_analysis.py --export check-en.jsonl --set xiaomi-loc-v1 --calibration-set cal-v1` gives Xiaomi's win rate (Xiaomi picks over Xiaomi plus LoC picks) with a page-clustered bootstrap interval, overall and by decade and damage band, the shares of neither, can't tell, not text and disputed, and for the calibration set how often the settled pick matches the truth.

## Loading a language

On the build host (pve-big for Cherokee and Yiddish), make the files. For Cherokee:

```sh
python3 tools/from_tap_data.py --data data.json --lang chr --set phoenix-nt-v1 \
  --language-config languages/chr.json --out-dir check-out
```

Then, from the operator's machine:

```sh
edge/check/load.sh --vault usnmtlskvprodffapzy --host root@pve-big.tjg.fyi \
  --dir /scratch/usnm-chr/testset/glyph-check/check-out --lang chr \
  --seed /scratch/usnm-chr/testset/glyph-check/check-out/seed.jsonl
```

For Yiddish and Serbian, `--dir` is `build_tasks.py`'s `--out-dir` ([above](#making-tasks-from-two-readings)) and `--lang yi` or `--lang sr`, with no `--seed`.

The files go from the build host straight to the Worker; only the admin token passes through the operator's machine, on ssh's stdin.

## Deploying

CI deploys the site: after `ci` passes on main, `.github/workflows/check-site-deploy.yml` runs `edge/check/deploy.sh`, which deploys when `edge/check` differs from what is live. The site serves `/build.txt`, the git tree hash of `edge/check` it was built from, so `curl https://check.usnewsmap.com/build.txt` against `git rev-parse origin/main:edge/check` shows whether main is live. To deploy again without a change, run the workflow by hand with "force". The script also runs from a clean checkout with `az` signed in to the vault (`edge/check/deploy.sh --force`).

Every run, whether it deploys or not, also sends `languages/*.json` to the site with `sync-languages.mjs`, so a merged change to a name, intro or letter note goes live with it. The site's `examples` are kept and the build options are left out, as `from_tap_data.py` does; a language the site doesn't have yet is skipped, since it needs its tasks and clips through `load.sh`. The CI identity reads `usnm-prod-check-admin-token` for this through the grant in `infra/modules/edge-worker-ci.bicep`. To see what a sync would change without sending anything: `node edge/check/sync-languages.mjs --dry-run`.

Changes to an existing database are in `migrations/`, run once in order: `0001-answers-notext.sql` must run before a page with "Not text" is deployed. The D1 schema is applied with `wrangler d1 execute usnm-check --remote --file schema.sql`, or through the D1 API; every statement is idempotent.

### Secrets

`SESSION_KEY` signs sessions, `ADMIN_TOKEN` guards `/admin/*`, and `TURNSTILE_SECRET` verifies the browser check. `set-secrets.sh` keeps the first two in Key Vault (`usnm-prod-check-session-key`, `usnm-prod-check-admin-token`) and sets all three on the Worker:

```sh
edge/check/set-secrets.sh --vault usnmtlskvprodffapzy --token-secret usnm-prod-workers-ci-token \
  --account-id <account>
```

Changing `SESSION_KEY` (`--rotate`) signs everyone out; their earlier answers stay.

## Tests

```sh
node --test edge/check/worker.test.mjs edge/check/layout.test.mjs edge/check/languages.test.mjs
```

The `check site` job in `ci.yml` runs these, a Wrangler dry-run build (`npx wrangler@4.154.0 deploy --dry-run`), shellcheck of the scripts and the tools' unit tests whenever anything under `edge/check/` but its Markdown changes. `languages.test.mjs` checks each language configuration's fields and that its keys are sorted.

`layout.test.mjs` renders every language's sample words in headless Chrome, one highlighted letter at a time, and fails if a highlight box overlaps another character. It skips when no Chrome is found (set `CHROME=/path/to/chrome`) unless `CI` is set.
