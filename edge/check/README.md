# check.usnewsmap.com

A public page where anyone can help check OCR text ([#1620](https://github.com/tgoodyear/usnewsmap.com/issues/1620)). Two readings of a printed word disagree, for example LoC's OCR and an aligned reference text. The page shows the printed word and both readings, and asks which one matches the print, or neither, or "can't tell", or "not text" when the box holds no letters (a picture or a smudge). Readers don't need to know the language: they compare letter shapes.

Cherokee (the _Cherokee Phoenix_, 1828–34) was the first language and Yiddish (New York dailies, 1910–53) the second. The data model assumes neither: a language is a configuration plus a set of tasks.

## Pieces

| Piece                         | What it is                                                                          |
| ----------------------------- | ----------------------------------------------------------------------------------- |
| `worker.mjs`                  | The Worker: `/api/*` for the page, `/clips/*` from R2, `/admin/*` for loading data. |
| `public/`                     | The page (static assets). `/` lists languages; `/<code>` (e.g. `/chr`) shows words. |
| D1 `usnm-check`               | `languages`, `tasks`, `answers` (`schema.sql`).                                     |
| R2 `usnm-check-clips`         | Line and word images, and the example letters, at `<lang>/<set>/...`.               |
| Turnstile widget `usnm-check` | The browser check before a session is issued.                                       |
| `tools/build_tasks.py`        | Turns line images with two readings per line into tasks, clips and a configuration. |
| `tools/yiddish_lines.py`      | Makes `build_tasks.py`'s input for Yiddish: LoC's ALTO against our `yidj2` model.   |
| `tools/from_tap_data.py`      | Turns a glyph-check data file (Cherokee) into tasks, clips and a configuration.     |
| `languages/<code>.json`       | A language's name, intro, writing direction and letter notes, plus build options.   |
| `load.sh`, `set-secrets.sh`   | Loading data and setting secrets, run by hand.                                      |

## Who answers, and how answers count

No account is needed. The first answer from a browser runs a Turnstile check, and the Worker issues a signed session holding a random id (30 days, `localStorage`). Answers are stored against that id, so one browser's answer to a word counts once. A later answer replaces it. Nothing else about the person is kept. One id may send at most 30 answers in 10 seconds.

A word **settles** once at least 3 answers are in and one choice has two thirds of them. That choice becomes its outcome: `a`, `b`, `neither`, `unsure` or `notext`; a tie goes to the first in that order. After 7 answers without that agreement, it closes as `disputed`. Settled words leave the queue. `/api/next` gives a person a word they haven't answered, choosing words nearest to settling first, so checked words come in steadily.

Seed answers (from `load.sh --seed`) are stored as contributor `seed:<name>` and count as one person.

Answers are published under CC0. `GET /api/export?lang=chr` returns one JSON line per word, with its readings, which is which (`a_is`, `b_is`), the count of each answer and the outcome. Contributor ids are never exported.

## API

| Method and path                           | Who         | What                                                                  |
| ----------------------------------------- | ----------- | --------------------------------------------------------------------- |
| `GET /api/config`                         | anyone      | The Turnstile site key.                                               |
| `GET /api/languages`                      | anyone      | Languages with task and settled counts.                               |
| `GET /api/languages/<code>`               | anyone      | A language's configuration.                                           |
| `POST /api/session`                       | anyone      | `{turnstile}` → `{session}`.                                          |
| `GET /api/next?lang=<code>[&skip=id,...]` | session     | `{task, answered}`; `task` is null when none is left for this person. |
| `POST /api/answers`                       | session     | `{task, choice}`: `a`, `b`, `neither`, `unsure` or `notext`.          |
| `GET /api/stats?lang=<code>`              | anyone      | Tasks, settled, answers, people.                                      |
| `GET /api/export?lang=<code>`             | anyone      | CC0 JSON lines (above).                                               |
| `PUT /admin/clips/<key>`                  | admin token | An image (JPEG, PNG or WebP).                                         |
| `PUT /admin/languages/<code>`             | admin token | A language's configuration.                                           |
| `POST /admin/tasks`                       | admin token | JSON lines of tasks; a task already loaded keeps its answers.         |
| `POST /admin/answers`                     | admin token | JSON lines of seed answers, `seed:` contributors only.                |

A task, as loaded: `id` (`<lang>/<set>/<word id>`), `lang`, `set`, then what the page shows: `page`, `source` (the page on Chronicling America), `label`, `line` and `word` (clip keys), `box` (the word in the line image, as fractions `[x, y, w, h]`), `a`, `b`, `a_diff`, `b_diff` (indexes of the letters that differ) and `a_is`, `b_is` (what each reading is, e.g. `reference` and `loc`). The page shows `a` and `b` in an order fixed by the task id, so neither side is always first.

## A language's configuration

`languages/<code>.json` is served at `/api/languages/<code>`, except for the build options, which the build tools remove:

| Field                           | What it is                                                                                    |
| ------------------------------- | --------------------------------------------------------------------------------------------- |
| `name`, `title`, `intro`        | Shown on the page.                                                                            |
| `dir`                           | `rtl` for right-to-left scripts (Hebrew letters); the readings are then set right to left.    |
| `notes`                         | A short note per letter, shown under a reading when that letter is highlighted.               |
| `drop_examples`, `min_examples` | Build options for `from_tap_data.py`'s example letter pictures.                               |
| `equivalents`                   | Build option for `build_tasks.py`: letters that print alike (`"װ": "וו"`), compared as equal. |

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

For Yiddish, `--dir` is `build_tasks.py`'s `--out-dir` ([above](#making-tasks-from-two-readings)) and `--lang yi`, with no `--seed`.

The files go from the build host straight to the Worker; only the admin token passes through the operator's machine, on ssh's stdin.

## Deploying

```sh
cd edge/check
CLOUDFLARE_ACCOUNT_ID=<account> CLOUDFLARE_API_TOKEN=<Workers token> npx wrangler@4.154.0 deploy
```

The D1 schema is applied with `wrangler d1 execute usnm-check --remote --file schema.sql`, or through the D1 API; every statement is idempotent.

### Secrets

`SESSION_KEY` signs sessions, `ADMIN_TOKEN` guards `/admin/*`, and `TURNSTILE_SECRET` verifies the browser check. `set-secrets.sh` keeps the first two in Key Vault (`usnm-prod-check-session-key`, `usnm-prod-check-admin-token`) and sets all three on the Worker:

```sh
edge/check/set-secrets.sh --vault usnmtlskvprodffapzy --token-secret usnm-prod-workers-ci-token \
  --account-id <account>
```

Changing `SESSION_KEY` (`--rotate`) signs everyone out; their earlier answers stay.

## Tests

```sh
node --test edge/check/worker.test.mjs
```
