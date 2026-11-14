# check.usnewsmap.com

A public page where anyone can help check OCR text ([#1620](https://github.com/tgoodyear/usnewsmap.com/issues/1620)). Two readings of a printed word disagree, for example LoC's OCR and an aligned reference text. The page shows the printed word and both readings, and asks which one matches the print, or neither, or "can't tell", or "not text" when the box holds no letters (a picture or a smudge). Readers don't need to know the language: they compare letter shapes.

Cherokee (the _Cherokee Phoenix_, 1828–34) was the first language, Yiddish (New York dailies, 1910–53) the second and Serbian (_Amerikanski Srbobran_, Pittsburgh, 1906–63) the third. The data model assumes none of them: a language is a configuration plus a set of tasks.

## Pieces

| Piece                          | What it is                                                                                                                  |
| ------------------------------ | --------------------------------------------------------------------------------------------------------------------------- |
| `worker.mjs`                   | The Worker: `/api/*` for the page, `/clips/*` from R2, `/admin/*` for loading data.                                         |
| `public/`                      | The page (static assets). `/` lists languages; `/<code>` (e.g. `/chr`) shows words.                                         |
| `public/fonts/`                | The Cherokee font the page serves, with its licence and source (`fonts/README.md`).                                         |
| D1 `usnm-check`                | `languages`, `tasks`, `answers` (`schema.sql`).                                                                             |
| R2 `usnm-check-clips`          | Line and word images, and the example letters, at `<lang>/<set>/...`.                                                       |
| Turnstile widget `usnm-check`  | The browser check before a session is issued.                                                                               |
| `tools/build_tasks.py`         | Turns line images with two readings per line into tasks, clips and a configuration.                                         |
| `tools/yiddish_lines.py`       | Makes `build_tasks.py`'s input for Yiddish: LoC's ALTO against our `yidj2` model.                                           |
| `tools/serbian_lines.py`       | Makes `build_tasks.py`'s input for Serbian: LoC's ALTO against our held BOINC ALTO.                                         |
| `tools/from_tap_data.py`       | Turns a glyph-check data file (Cherokee) into tasks, clips and a configuration.                                             |
| `tools/phoenix_lines.py`       | Picks Cherokee lines from LoC's ALTO and our production re-read for `build_tasks.py`.                                       |
| `tools/lookalike.py`           | Finds letters of two readings the page draws almost alike, and pairs that differ only in a mark; also a command on exports. |
| `tools/mark_report.py`         | Compares answers on words that differ only in a mark with other words, from exports.                                        |
| `tools/cherokee_labels.py`     | Writes (or checks) the Cherokee letter names in `languages/chr*.json` from the Unicode character names.                     |
| `tools/names_report.py`        | Compares answers given with letter names showing and without, from exports.                                                 |
| `tools/recompute_marks.py`     | Recomputes `mark_only` and `lookalikes` on task bodies already loaded, with a report; loads them only when told to.         |
| `tools/merged_lines.py`        | The merged-line rule (a TextLine box over 1.6 times its page's median) the three line tools share.                          |
| `tools/merged_lines_loaded.py` | Lists loaded tasks cut from a merged line, measured from LoC's ALTO in the bulk OCR archives; read-only.                    |
| `languages/<code>.json`        | A language's name, intro, writing direction and letter notes, plus build options.                                           |
| `confusable.sh`                | Marks tasks (`confusable_display` or `merged_line`), or reopens `confusable_display` ones, run by hand.                     |
| `load.sh`, `set-secrets.sh`    | Loading data and setting secrets, run by hand.                                                                              |

## Who answers, and how answers count

No account is needed. The first answer from a browser runs a Turnstile check, and the Worker issues a signed session holding a random id (30 days, `localStorage`). Answers are stored against that id, so one browser's answer to a word counts once. A later answer replaces it. Nothing else about the person is kept. One id may send at most 30 answers in 10 seconds.

A word **settles** once at least 3 answers are in and one choice has two thirds of them. That choice becomes its outcome: `a`, `b`, `neither`, `unsure` or `notext`; a tie goes to the first in that order. After 7 answers without that agreement, it closes as `disputed`. Settled words leave the queue. `/api/next` gives a person a word they haven't answered, choosing words nearest to settling first, so checked words come in steadily.

Seed answers (from `load.sh --seed`) are stored as contributor `seed:<name>` and count as one person.

Answers are published under CC0. `GET /api/export?lang=chr` returns one JSON line per word, with its readings, which is which (`a_is`, `b_is`), the count of each answer and the outcome. Contributor ids are never exported. Each word also has `font_counts`, the same answers counted by what the visitor's page reported about the bundled font ([below](#which-font-each-answer-was-given-with)) and `names_counts`, the same answers counted by whether letter names were showing ([below](#letter-names)). A word with look-alike letters also has its `lookalikes`, and one whose readings differ only in a mark on the same letter has `mark_only`; a marked word also has `mark_reason`, `held`, `excluded_counts`, `excluded_font_counts` and `excluded_names_counts` (and `confusable_display: true` when the reason is `confusable_display`; [below](#words-whose-readings-look-alike)), and one that was settled when it was marked has `outcome_withdrawn`, `withdrawn_reason` and `withdrawn_at` ([below](#words-cut-from-a-merged-line)).

## API

| Method and path                           | Who         | What                                                                                                                                                                                                                          |
| ----------------------------------------- | ----------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `GET /api/config`                         | anyone      | The Turnstile site key.                                                                                                                                                                                                       |
| `GET /api/languages`                      | anyone      | Languages with task and settled counts.                                                                                                                                                                                       |
| `GET /api/languages/<code>`               | anyone      | A language's configuration.                                                                                                                                                                                                   |
| `POST /api/session`                       | anyone      | `{turnstile}` → `{session}`.                                                                                                                                                                                                  |
| `GET /api/next?lang=<code>[&skip=id,...]` | session     | `{task, answered}`; `task` is null when none is left for this person.                                                                                                                                                         |
| `POST /api/answers`                       | session     | `{task, choice[, display, font, names_shown]}`: `a`, `b`, `neither`, `unsure` or `notext`; `font` and `names_shown` are optional ([font](#which-font-each-answer-was-given-with), [names](#letter-names)).                    |
| `GET /api/stats?lang=<code>`              | anyone      | Tasks, settled, answers, people.                                                                                                                                                                                              |
| `GET /api/export?lang=<code>`             | anyone      | CC0 JSON lines (above).                                                                                                                                                                                                       |
| `PUT /admin/clips/<key>`                  | admin token | An image (JPEG, PNG or WebP).                                                                                                                                                                                                 |
| `PUT /admin/languages/<code>`             | admin token | A language's configuration. With `?keep=examples` (an existing language only), the stored `examples` stay.                                                                                                                    |
| `POST /admin/tasks`                       | admin token | JSON lines of tasks; a task already loaded keeps its answers.                                                                                                                                                                 |
| `POST /admin/answers`                     | admin token | JSON lines of seed answers, `seed:` contributors only.                                                                                                                                                                        |
| `POST /admin/confusable`                  | admin token | `{action: "mark" or "reopen", tasks: [ids], reason?}`, up to 500 ids; `reason` is `confusable_display` (default) or `merged_line` ([below](#words-whose-readings-look-alike), [merged lines](#words-cut-from-a-merged-line)). |

A task, as loaded: `id` (`<lang>/<set>/<word id>`), `lang`, `set`, then what the page shows: `page`, `source` (the page on Chronicling America), `label`, `line` and `word` (clip keys), `box` (the word in the line image, as fractions `[x, y, w, h]`), `a`, `b`, `a_diff`, `b_diff` (indexes of the letters that differ), `a_is`, `b_is` (what each reading is, e.g. `reference` and `loc`) and, when the build found letters the page draws almost alike, `lookalikes` and `mark_only` (see below). The page shows `a` and `b` in an order fixed by the task id, so neither side is always first.

## A language's configuration

`languages/<code>.json` is served at `/api/languages/<code>`, except for the build options, which the build tools remove:

| Field                           | What it is                                                                                                                                                                                                                                                                          |
| ------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `name`, `title`, `intro`        | Shown on the page. `intro` covers the source and anything specific to the script; the answer rules every language shares are in `public/index.html`.                                                                                                                                |
| `dir`                           | `rtl` for right-to-left scripts (Hebrew letters); the readings are then set right to left.                                                                                                                                                                                          |
| `notes`                         | A short note per letter, shown under a reading when that letter is highlighted.                                                                                                                                                                                                     |
| `labels`                        | A short name per letter: the label under each reading of a flagged look-alike pair (`"п": "p"` in Serbian; a letter with no entry keeps the stored name), and the name under each letter when a visitor turns on "Show letter names" ([below](#letter-names)). See "A label" below. |
| `marks`                         | A name per combining mark, for the label the page shows when two readings differ only in that mark (`"\u05b7": "pasekh"`, written as the character). A mark with no name shows as "another mark", never its Unicode name.                                                           |
| `drop_examples`, `min_examples` | Build options for `from_tap_data.py`'s example letter pictures (not served).                                                                                                                                                                                                        |
| `equivalents`                   | Letters that print alike (`"װ": "וו"`): `build_tasks.py` compares them as equal, and the page sets the two-letter spelling with no space inside it.                                                                                                                                 |

The page highlights letters as `a_diff` and `b_diff` count them: a base character with the marks that follow it (פּ, אַ) is one letter.

## Words whose readings look alike

Some readings differ only in letters that a font draws the same way. On 13 November 2026 the page's font stack fell, on macOS, to a serif Cherokee font that draws Ꭱ (e) and Ꮢ (sv) as one R, so a person could not pick between ᏒᎯ and ᎡᎯ ([#1939](https://github.com/tgoodyear/usnewsmap.com/issues/1939)). Four parts deal with it.

**A served font.** `public/fonts/NotoSansCherokee-Regular.woff2` (SIL OFL; source, version and hashes in `public/fonts/README.md`) is first in `--font-script`, for the Cherokee blocks only, so Cherokee readings don't depend on installed fonts. The site serves no font for the other scripts: Hebrew letters and Cyrillic still depend on what the visitor has.

**A check at build.** `tools/lookalike.py` takes each pair of letters that differ at the same position between a task's two readings (readings of the same length are compared position by position, so a transposition gives two pairs), draws both in the font the page asks for first for that script, at the size the page shows them (32px), and flags the pair when the two pictures nearly coincide: intersection over union of the inked pixels, the best of one-pixel shifts, at least `THRESHOLD` (0.82). The threshold sits between the bundled font's closest pair of Cherokee letters (0.80, Ꮐ and Ᏻ; Ꭱ and Ꮢ score 0.71) and the pairs macOS's Plantagenet Cherokee draws alike (Ꭱ and Ꮢ 0.85); the table it was set from is in `tools/test_lookalike.py`. For a script the site serves no font for, the result only describes the reference font used (the first of the stack's families installed on the build host, or `--font SCRIPT=PATH`): a visitor with another font sees other shapes. A pair with no font or no glyph is counted, never guessed. `build_tasks.py` and `from_tap_data.py` write the flagged pairs into the task as `lookalikes`: `[{"a": 0, "a_label": "sv", "b": 0, "b_label": "e", "iou": 0.714}]`, the letter indexes as `a_diff` and `b_diff` count them. Tasks already loaded get the field when their tasks file is built again and loaded; `/admin/tasks` keeps their answers.

To measure tasks already loaded, without rebuilding, run the same check on exports (`--font` repeats; `--ids` lists the tasks; `--threshold 1.0` flags only identical pictures):

```sh
for l in chr chr-lines chr-rare yi sr; do curl -s "https://check.usnewsmap.com/api/export?lang=$l" > check-$l.jsonl; done
python3 edge/check/tools/lookalike.py --export check-chr.jsonl --export check-chr-lines.jsonl --export check-chr-rare.jsonl --ids
python3 edge/check/tools/lookalike.py --export check-chr.jsonl --font Cherokee=PlantagenetCherokee.ttf   # the old macOS fallback
```

**Letters with combining marks.** A letter is a base character with its marks (פּ, אַ), compared as one unit, never a mark alone. Drawing it needs shaping: Pillow's basic layout engine sets the mark beside its letter. `lookalike.py` therefore draws a pair with a mark through Raqm (HarfBuzz, as a browser shapes it), and counts the pair in `unshaped_pairs`, not measured, when its Pillow has no Raqm. Run the command with a Pillow that has Raqm (`python3 -c "from PIL import features; print(features.check('raqm'))"`) for Yiddish. For the Yiddish export this changed no number, because a mark that doesn't overlap its letter adds the same ink wherever it sits; a mark inside the letter would.

**Measured on 13 November 2026, no font added for Yiddish or Serbian.** On the exports of that day (720 Yiddish tasks, 939 Serbian), at the 0.82 threshold:

| Yiddish reference font                        | Tasks flagged | Pairs flagged | What the flagged pairs are                                                             |
| --------------------------------------------- | ------------: | ------------: | -------------------------------------------------------------------------------------- |
| Times New Roman (what the page uses on macOS) |           193 |            30 | Only a letter against the same letter with a point: אַ/אָ, א/אַ, א/אָ, פ/פּ, פֿ/פּ, ב/בּ, ך/ךּ… |
| Noto Serif Hebrew 2.004 (notofonts, OFL)      |           194 |            32 | The same, plus יַ/יִ and ן/ןָ.                                                            |
| Noto Sans Hebrew 3.001 (notofonts, OFL)       |           213 |            34 | The same as Noto Serif Hebrew, plus ח/ה (both directions, 20 pairs).                   |

No candidate font reduces the set, so none is bundled. The flagged pairs are all a letter with a small point against the same letter without it or with another point: the point is a few percent of the ink, so any font scores them high. In Chrome at 32px, in all three fonts, the points are drawn on the letters and told apart: patah (a bar) and qamats (a bar with a stem), dagesh and rafe in פּ and פֿ, hiriq under י, holam over ו, and the shin and sin dots. The presentation forms (U+FB1D to U+FB4E: אַ, אָ, פּ, פֿ, בּ, וּ, יִ, ײַ) draw the same as the letter plus mark in all three fonts; the tasks hold only the decomposed form, since the build normalises to NFC. So the overlap measure flags these pairs for what they are, a small mark, and says nothing about whether it shows. Whether the mark is visible in the worn print itself is the person's judgment on the clip; no font changes that.

Serbian, Times New Roman, 62 of 939 tasks: п/н 45 (regular 0.88, italic 0.74), п/ц 6, н/ц 4, Ц/П 2, П/Н 2, И/Н 2, and one each of П/H (Latin H), ћ/h and п/џ. Each of these pairs shares a skeleton and differs by one stroke: п and н by the crossbar, п and ц by the tail, ц and н by both, П and Н by the crossbar, И and Н by the diagonal, ћ and h by the bar through the stem. At 32px in Times New Roman, regular and italic, the stroke shows in every pair. They are look-alikes in small or worn type (п/н, ћ/h, П/Н, where the one stroke is the whole difference), and detector noise elsewhere: overlap counts shared verticals, and an italic н or ц already scores 0.58 to 0.74 against п. The pages measured were not checked against the print.

**Letters that differ only in a mark.** When the two letters of a differing pair have the same base character and differ only in combining marks (אַ and אָ, פּ and פֿ, א and אַ; one may have none), the build skips the overlap check and records the pair as `mark_only`, in any script. A letter is a base character with the combining marks that follow it after the build's NFC normalisation, so an accent the readings keep precomposed (é, й) is its own base letter and keeps the overlap check: that keeps Serbian and the other precomposed scripts as they were. A mark is a few percent of the ink, so the overlap measure flags such a pair in every font, and says nothing about whether the mark shows. Pairs whose base letters differ keep the overlap check. A task gets `mark_only: [{"a": 1, "a_marks": "ַ", "b": 1, "b_marks": "", "base": "א", "differ": "ַ"}]`: the letter indexes as `a_diff` and `b_diff` count them, the mark characters of the letter the page displays at that index in each reading (`""` for none; a label names these), the base character, and `differ`, the marks that tell the two apart. Pairs are judged on the letters as displayed first, then on letters expanded through the language's `equivalents` (ײַ is ייַ in Yiddish), so ײַ against ײָ is one pair though the expanded readings differ in length, and a pair found through an expansion takes its marks from the displayed letter, never "no mark" for a pointed one. Tasks built before `differ` existed lack it; the report then works it out from `a_marks` and `b_marks`. Keys are in ascending order. The field is separate from `lookalikes`, so both can be counted; `lookalike.py`'s report (and its `--json`) gives, per language, `mark_only_tasks`, `mark_only_pairs` and `mark_only_by_mark` (by the mark that differs), and `build_tasks.py` and `from_tap_data.py` print how many tasks have it. The export carries `mark_only` as stored.

Under each reading of such a pair the page shows a short label naming the marks that reading has on that letter, and the letter (base and marks, as one cluster) at about twice the size of the highlight chip in a small box beside the label, so a small mark can be seen. A reading with no mark on that letter says "no mark". Names come from the language's `marks` ([above](#a-languages-configuration)); the label is text set left to right, and the enlarged letter is `aria-hidden` because the label already says what the letter is. `layout.test.mjs` renders it in right-to-left text and checks that no part of a row overlaps another or leaves the column. Nothing is marked `confusable_display` for a mark-only pair, and no answer is set aside.

`languages/yi.json` names the marks in YIVO romanisation where a source gives it: pasekh (patah), komets (qamats), khirik (hiriq) and khoylem (holam), as the alphabet table of Wikipedia's "Yiddish orthography" article spells them (also "pasekh alef", "komets alef" and "khirik yud" on verterbukh.org's alphabet page, `doc/en/alefbeys.html`). The same article spells the other two marks the page names in the common English form, dagesh and rafe; no source checked gives the YIVO forms "dogesh" and "rofe", so the file does not use them. tsere and segol (in the Yiddish export, rarely) are named in the common English form for the same reason. The shin dot and the sin dot are "shin dot" and "sin dot". A language adds its own `marks`; without one, the label is "another mark".

On the Yiddish export of 14 November 2026 (720 tasks, Times New Roman as the reference font, Pillow with Raqm): before this change 193 tasks and 209 pairs were flagged by overlap; after it 0 are, and 195 tasks with 215 pairs are `mark_only`. By the mark that differs (a pair can differ in two): patah 96, qamats 89, dagesh 51, rafe 15, hiriq 5, holam 5, tsere 4, segol 2, shin dot 2, sin dot 1. Judging pairs on displayed letters changed 2 of the 720 tasks compared with the first version of this rule, which paired letters only after expansion: one gained a pair it had missed and one three, in readings whose lengths differed once ײ or ױ expanded.

Whether a mark can be told apart in print is for the answers to say. `tools/mark_report.py` takes exports and, per language, mark and font group, gives the can't-tell rate (`unsure` over all answers) and the pairwise agreement (the share of pairs of answers to one word that chose the same thing) for mark-only words, beside the same two figures for words flagged by neither rule. The font group comes from the word's `font_counts` (the value all its answers carry, `mixed` when they differ, `unknown` when the export has none). A row with under 20 answers prints "too few answers" and no rates. It stops, unless `--allow-incomplete`, when some letter pairs could not be checked for look-alikes (no reference font or glyph, or no Raqm for a mark), since those words would land in the unflagged group. On 14 November 2026 no mark has that many answers in Yiddish.

```sh
python3 edge/check/tools/mark_report.py --export check-yi.jsonl [--min-answers 20] [--json]
```

_Tasks already loaded_ were built before this field. They keep working: the page shows no mark label for a task without `mark_only`, and the old behaviour for them stays (a long Unicode-name label for any pair in their `lookalikes`). To add `mark_only` and recompute `lookalikes` on the loaded tasks, run `tools/recompute_marks.py` ([Updating loaded tasks](#updating-loaded-tasks)): it needs no build inputs, because each stored task has both readings. The `marks` names reach the site with the deploy (`sync-languages.mjs`), so update the tasks after the deploy. The run needs a Pillow with Raqm wherever readings carry combining marks, as every Yiddish run does.

**A label.** Under each reading of a task with `lookalikes`, the page names the flagged letter next to its highlight. The name is the letter's entry in the language's `labels` if it has one (`languages/sr.json` gives the Latin equivalent of every Serbian Cyrillic letter, both cases: п is `p`, џ is `dž`, Њ is `Nj`). A Latin letter in a language that has `labels` is `Latin <letter>`. Otherwise it is the name the task stored: the syllable for Cherokee (`CHEROKEE LETTER SV` gives `sv`), else the Unicode character name (`hebrew letter alef + hebrew point patah`). When one reading of a pair is a Latin letter and the other is Cyrillic, the Cyrillic letter also names its script, so the labels never read as one letter twice: П against Latin H shows `Cyrillic P` and `Latin H`, ћ against Latin h shows `Cyrillic ć` and `Latin h`. The labels come from the language file, so they reach the site with the deploy; a task needs `lookalikes` in its body to show one. A flagged letter is shown even where the highlight doesn't mark it (a transposition). The label is set left to right inside right-to-left readings. The page also keeps the answer buttons off, and offers a reload, when a Cherokee word's font fails to load, so a visitor's installed font can't stand in for it. `layout.test.mjs` renders it in Chrome for Cherokee, Hebrew letters and Cyrillic. For pairs that differ only in a mark, see above: they are not in `lookalikes`.

**Answers given before the fix.** A task marked `confusable_display` keeps its existing answers, but they no longer count. `POST /admin/confusable` (run with `confusable.sh`) does two things:

- `mark`: for each task not yet marked, in one transaction, the task's answers move to `answers_excluded` (kept, with their contributor, choice and time, and in the backup dump), the copy is checked against the original, and the task is set `confusable_display = 1`, `held = 1`, with `n = 0`, unsettled and no outcome. A held task is not served by `/api/next` and `/api/answers` answers 409. Marked tasks are not marked again, so a second run can't set aside answers given after the fix.
- `reopen`: a held marked task becomes open for new answers (`held = 0`). The set-aside answers stay set aside. A person who answered before is asked again, since the one answer per person and word is now in `answers_excluded`; their new answer is stored as any other.

A marked task takes answers only from a page at the current display revision: the page sends `display` with each answer (`DISPLAY` in `public/app.js`) and the Worker answers 426, storing nothing, when a marked task's answer has a lower one (`DISPLAY_REVISION` in `worker.mjs`; a test keeps the two equal). That keeps a tab opened before the font was served from answering about letters it draws alike. The check is in the answer's SQL, so it holds against a request that read the task before it was marked. Loading a seed file again skips a seed answer already set aside for a marked task (same contributor and time; `/admin/answers` reports `skipped`). Tasks never marked take answers from any page. Marked tasks settle only on new answers, by the same rule as any task. Set-aside answers are in no count, `/api/stats` figure or settled outcome. In the export a marked task has `confusable_display: true`, `held`, and `excluded_counts` (the same five choices); `counts` are only the new answers. `docs/reports/2026-11-cherokee/scripts/chr_disputes_score.py` reads only `counts` and treats a held marked word as waiting. Tasks that were never marked behave as before.

After merging, in this order:

1. Before the merge, run `migrations/0002-confusable-display.sql` on D1 (it only adds two columns and a table, so the running Worker is unaffected). The deploy does not run migrations, and the new Worker fails without them.
2. Merge. CI deploys the Worker and page with the font, and sends `languages/*.json`. Nothing else changes: no task is marked and no data is loaded.
3. Check `curl https://check.usnewsmap.com/build.txt` against `git rev-parse origin/main:edge/check`, and that `/fonts/NotoSansCherokee-Regular.woff2` loads.
4. Put the task ids in a file and run `edge/check/confusable.sh --vault usnmtlskvprodffapzy --action mark --tasks ids.txt`, then `--action reopen` with the same file. Marking and reopening can be one after the other: the point of the pause is only to look at the export (`excluded_counts`) first.
5. Rebuild and reload a language's tasks, as [below](#loading-a-language), to add `lookalikes` and their labels to tasks already loaded. Nothing needs this to be right: without `lookalikes` a task shows no label.

## Words cut from a merged line

LoC's layout sometimes boxes two printed lines as one `TextLine`. A task cut from it has a line clip and a word crop that include the line below: the word box takes the line's height, so the enlarged word shows the target and the start of the next line. The readings are for the top line only ([#1978](https://github.com/tgoodyear/usnewsmap.com/issues/1978)). Example: `chr/phoenix-nt-v1/sn83020866_1828-05-28_ed-1_seq-3_P3_TL00018~0`. A count from clip heights (clip taller than 1.6 times the median clip of its page) before this change: `chr/phoenix-nt-v1` 2 clips and 7 words, `chr-lines/disputes-v1` 7 and 7, `chr-rare/rare-v1` 8 and 9, `chr-rare/nah-v1` 0, `sr` 3 and 8, `yi` 23 and 54. The exact counts come from the tool below.

**The rule** (`tools/merged_lines.py`). A `TextLine` is a merged line when its box height is more than `--merged-line-factor` (default 1.6) times the median `TextLine` height of its page. The median is over every line of the page's ALTO, not only the lines a build picks. A page with fewer than `--min-page-lines` (default 5) lines of positive height is not checked: a median over so few lines can be a merged line itself, and one tall line against three short ones says nothing about the page. Exactly 1.6 times the median is not flagged. Heights are in the ALTO's own units, so no scale is needed. `tools/test_merged_lines.py` tests both edges.

**At build.** No task is made from a merged line. The rule is in every path that reads ALTO line boxes:

- `tools/phoenix_lines.py` (all three modes: `disputes`, `rare`, `nah`): the median is over all of LoC's lines on the page, and a flagged line is removed before candidates are chosen. Its report (`report.json` and stderr) counts them in `merged`, per page and in total.
- `tools/yiddish_lines.py`: a flagged line is dropped from the lines the protocol picks (the anchor's line and the two after it); the run prints the count per page and in total.
- `tools/serbian_lines.py`: a flagged line is not a candidate, so the random pick falls on the others. The box is LoC's, which is reading a's.
- `tools/build_tasks.py` has no line boxes of its own, since it reads the tools' output. It leaves out and counts any input row with `"merged_line": true`, which guards a hand-made lines file.
- `tools/from_tap_data.py` is not covered: its input (`data.json` of the tap page) holds line images and word boxes, not ALTO line boxes. `chr/phoenix-nt-v1` came from it and is measured by the loaded-task tool.

Each tool writes `merged-lines.jsonl` (the flagged lines with page, line id, box, page median, ratio, factor and text) next to its `lines.jsonl`: `OUT_DIR/<kind>/merged-lines.jsonl` for `phoenix_lines.py`, `DIR/merged-lines.jsonl` for the other two. The archive step in [Keeping build inputs](#keeping-build-inputs) copies it, so the lines left out can be traced.

**Loaded tasks: finding them.** The build inputs of the loaded sets were not kept, but a task id names its LoC line, and LoC's ALTO for every page is in the bulk OCR archives (`<batch>_verNN.tar.bz2`, members `<lccn>/<yyyy>/<mm>/<dd>/ed-<n>/seq-<m>/ocr.xml`, [#1907](https://github.com/tgoodyear/usnewsmap.com/issues/1907)). `tools/merged_lines_loaded.py` reads the task bodies from a dump, finds each task's `TextLine` and applies the rule to the line's box against its page's median. It uses the Python standard library only, takes named flags and never contacts loc.gov.

- Which line: for `chr`, `chr-lines` and `chr-rare` the task id ends in the ALTO `TextLine` ID (`..._seq-3_P3_TL00018`). For `yi` and `sr` it ends in `_l<N>`, the line's index in `yiddish_lines.py`'s order (TextBlock by TextBlock); a test checks that order. For `sr`, reading a is LoC's ALTO and so is the box used here. Reading b's own layout (the held BOINC ALTO) is not read.
- Which archive: `--pages docs/reports/2026-11-cherokee/phoenix-pages.jsonl` names the batch of each of the 324 Cherokee pages (two archives: `dlc_misctopsn83020874_ver02` for 240 pages and `dlc_misctopsn83020866_ver03` for 84). For Yiddish and Serbian the repository has no page-to-archive list, so `--archive-map` takes a TSV of `doc_id` and `archive` from `quality.loc_alto` (columns `doc_id`, `archive`). Only the archives that hold a wanted page are opened. A page listed in several archives takes the first one that holds it, in the order `--pages` then `--archive-map` list them, though every listed archive is read. Each is streamed once and the read stops when its wanted pages are found; `--workers` reads that many at once.
- Fallback: a task whose page is not found in the archives (no archive listed, the archive absent from `--archives`, the page or line not in the ALTO) uses its line clip's pixel height instead (`--clips DIR` of clips at their keys, or `--clips-url` to fetch the missing ones from the public `/clips/<key>` one at a time, `--delay` seconds apart). The three line tools pad a clip in proportion to the box height (15% each side; Cherokee lines have a 4-pixel minimum, which only matters under 27 pixels) and `build_tasks.py` saves it unscaled, so clip heights keep the ratio of box heights except for a line cut short at the page edge. The median is then over the clips of the page's distinct lines in that one set (scan scales differ between sets), counting lines the ALTO did measure, so it needs 5 distinct lines of that page in the set, and most pages will have fewer: those tasks are not flagged and are counted as `page_too_few_lines`. The report counts tasks measured from the ALTO, from clips and unmeasured separately.
- Output, in `--out-dir`: `affected-task-ids.txt` (the file `confusable.sh --tasks` reads), `flagged-lines.jsonl`, `unmeasured-tasks.txt`, `report.json` and `report.txt`. Per set the report gives the tasks, how each was measured, the flagged lines and tasks, the answers on them (active and already set aside), the settled ones by outcome and how many were already marked.
- Time: a page is found by reading its archive's stream, so one archive costs a pass through its bzip2 data. LoC's 3,011 archives hold 2.47 TB, about 0.8 GB each on average. Python's `bz2` decompresses roughly 30 to 50 MB of output a second per core (estimated, not measured on these archives), which is a few minutes for an archive read to its end. The two Cherokee archives take minutes. Yiddish and Serbian depend on how many archives their 52 and 134 pages are spread over; at about fifty archives and eight workers expect 20 to 40 minutes. Run one `--lang` at a time to see the pace (the tool prints each archive as it finishes). A Rust reader would not decompress faster, since it also goes through bzip2; it was not built.

Run on the storage host (read-only on the site; the dump needs the admin token from Key Vault):

```sh
python3 edge/check/tools/merged_lines_loaded.py --vault usnmtlskvprodffapzy --lang chr --lang chr-lines --lang chr-rare \
  --archives /path/to/bulk-ocr --pages docs/reports/2026-11-cherokee/phoenix-pages.jsonl --workers 4 --out-dir merged-lines-chr
python3 edge/check/tools/merged_lines_loaded.py --dump dump.jsonl --lang yi --lang sr --archives /path/to/bulk-ocr \
  --archive-map doc-archives.tsv --workers 8 --out-dir merged-lines-yi-sr
```

`doc-archives.tsv` comes from `quality.loc_alto`: `\copy (SELECT doc_id, archive FROM quality.loc_alto WHERE doc_id IN (...)) TO 'doc-archives.tsv'` for the task pages, or the whole table. With `--dump FILE` the tool needs no network. Nothing is marked by the tool.

**Setting answers aside, with a reason.** `confusable.sh --action mark --reason merged_line --tasks affected-task-ids.txt` marks the tasks. A mark has a reason: `confusable_display` (the default, the existing meaning) or `merged_line`. For either, in one transaction per task: the task's answers move to `answers_excluded` (kept, in the backup dump), the copy is checked against the original, and the task is held out of the queue with `n = 0`, `settled = 0` and `outcome = NULL`, so it counts as no checked text. A task that was settled keeps the outcome it had in `outcome_withdrawn`, with `withdrawn_reason` and `withdrawn_at` (unix milliseconds); one that was not settled has none of the three. Nothing is deleted. The differences between the reasons:

- `confusable_display` sets `confusable_display = 1`, can be reopened with `--action reopen`, and takes new answers only from a page at the current display revision.
- `merged_line` leaves `confusable_display = 0` and sets only `mark_reason = 'merged_line'` and `held = 1`. It is not reopened: `reopen` on it does nothing, and a held task answers 409. A task already marked `merged_line` is not marked again, and a `confusable_display` task is not marked `confusable_display` again, so a second run can't rewrite `withdrawn_at`. A task marked `confusable_display` can be marked `merged_line`: it changes reason and stays held; if it was reopened and took answers, those move to `answers_excluded` as well and a settled outcome is withdrawn. That move is refused, and the task listed under `unchanged`, when it could not keep everything: a current answer whose contributor already has a different set-aside answer (`answers_excluded` has one place per person and word), or a settled outcome on a task that already holds a withdrawn one. `merged_line` never goes back to `confusable_display`.
- A task marked before `mark_reason` existed (the four `confusable_display` ones) has `mark_reason` NULL and `confusable_display = 1`. The Worker and the export read it as `confusable_display`, and nothing is changed on it.

The export gives a marked task `mark_reason`, `held`, `excluded_counts` (and the font and names counts), `settled: false`, `outcome: null`, and, if it had settled, `outcome_withdrawn`, `withdrawn_reason` and `withdrawn_at`. `/api/stats` and the language list count it in no settled or answer figure, since its `settled` and `n` are zero. Readers of `outcome` and `settled` in the repository: `docs/reports/2026-11-cherokee/scripts/chr_disputes_score.py` puts a line with a `merged_line` word in state `excluded`, in neither the resolved nor the waiting lines (tests in `test_chr_disputes_score.py`); `tools/en_pick_analysis.py` reads only settled rows; `crates/usnm-glyphs` (`add-checked`) uses only settled `a` or `b` words, so a marked word adds no glyphs, and a bank that already holds the glyphs of a word marked later fails `add-checked` with its "start again from a bank without it" message, as for any word that loses its consensus. That crate was not changed. `mark_report.py` and `names_report.py` read `counts`, which are zero for a marked word.

**Migration 0005 is needed before the Worker that marks with a reason is deployed.** `migrations/0005-mark-reason.sql` adds four nullable columns to `tasks` (`mark_reason`, `outcome_withdrawn`, `withdrawn_reason`, `withdrawn_at`). It only adds, so the running Worker is unaffected, and **it must run before the merge**: the deploy does not run migrations, and the new Worker fails without the columns. After merging, in this order:

1. Before the merge, run `migrations/0005-mark-reason.sql` on D1.
2. Merge. CI deploys the Worker.
3. Check `curl https://check.usnewsmap.com/build.txt` against `git rev-parse origin/main:edge/check`.
4. Run `merged_lines_loaded.py` on the storage host (above) and look at `report.txt`.
5. Mark the affected tasks: `edge/check/confusable.sh --vault usnmtlskvprodffapzy --action mark --reason merged_line --tasks affected-task-ids.txt` (500 ids a request; the script sends the file as one request, so split a longer one). With a Worker from before step 2 the script fails because the reply lacks the reason.

No clip is recropped or regenerated. A later set that recrops the merged lines would need the scans. Where they may be: the Serbian scans are on the storage host (verified 14 November 2026, 134 of 134 pages). The Cherokee pages may be on the host that serves the Phoenix site's local page copies (not verified). The Yiddish pages may be under the kept-scans directory (not verified).

## Letter names

Under the readings is a checkbox, "Show letter names", off by default. When it is on, each letter that has a name is set in a small cell with its name centred under it: for Cherokee, the syllable (a, e, i, o, u, v, ga, ka, ... sv, ... mv), for every letter of both readings, in `chr`, `chr-lines` and `chr-rare`. The choice is kept in this browser (`localStorage`, key `usnm-check-names`, read and written inside `try`/`catch`: with storage blocked the page works and keeps the choice until it is closed) and applies at once, without reloading or leaving the word.

**Where names come from.** The language's `labels` map, the same map the look-alike label reads (above), so there is one source for both. The Cherokee map is generated, never typed: `tools/cherokee_labels.py --write` takes the syllable from each letter's Unicode character name (`CHEROKEE LETTER SV` is `sv`; it uses `lookalike.label()`, the derivation the build stores with a flagged pair) for the 86 letters U+13A0 to U+13F5, and `--check` fails when a file differs. The three `chr` files carry the same map, in sorted order. The public exports of 14 November 2026 (`chr`, `chr-lines`, `chr-rare`) use 84 of those 86 letters, and no lowercase Cherokee letter (U+13F8 to U+13FD, U+AB70 to U+ABBF); the map has no entry for those, so one would show no name. Serbian's `labels` (the Latin equivalent of each Cyrillic letter) make the option appear there too, with the Latin letter under the Cyrillic one. A language with no `labels`, Yiddish today (it has `marks`, which name combining marks, not letters), does not show the control.

**Layout.** With the option off the markup and the layout are exactly what they were. With it on, a named letter is an inline cell (letter, then name below), as wide as the wider of the two, with half the usual letter gap on each side; letters without a name (spaces, digits, brackets) stay plain text. Names are wider than some letters (tla, quv), so the letters are spaced by cell, not by the usual advance. Highlights are still the letter's own box. The name is small text in the page's UI font, set left to right inside right-to-left readings, and cannot overlap a letter or another name because each sits in its own flex cell.

**Reading order and copying.** Each name is real text after its letter, so a screen reader reads a letter, then its name. The name has `user-select: none`, so selecting and copying a reading gives the letters only, where the browser honours that.

**Look-alike labels.** A flagged letter already shows its name in the row of pictures below the reading. With the option on, that label is not printed again when it is the same text as the name under the letter (all Cherokee labels); a label that adds something, Serbian's "Cyrillic P" against "Latin H", stays.

**What is stored.** Each answer stores `names_shown`, whether the page was showing names when it was given (`NAMES_STATES` in `worker.mjs`, a `CHECK` on the column):

| Value       | Meaning                                                                                                                                                                          |
| ----------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `shown`     | The option was on, and at least one letter of either reading had a name under it.                                                                                                |
| `not_shown` | The option was off; or the word had no named letter; or the language has no letter names, so the control is not offered (the page then sends `not_shown`, names were not shown). |
| `unknown`   | The answer is older than the column, the page is from before it (a cached page sends none), or the request sent anything else. Seed answers are `unknown`.                       |

Nothing else about the visitor is stored. An answer changed later takes the new answer's value; a set-aside answer keeps it in `answers_excluded`. The export gives each word `names_counts` (`shown`, `not_shown`, `unknown`, adding up to the total of `counts`) and, on a `confusable_display` word, `excluded_names_counts`. `counts`, `font_counts` and the score scripts are unchanged. The nightly backup reads `SELECT *`, so it carries the new column without a change.

`migrations/0004-answer-names.sql` adds the column to `answers` and `answers_excluded` with `DEFAULT 'unknown'`. It only adds: the Worker from before it keeps working on the migrated database, so **run it before the merge**; the new Worker fails without it.

**Analysis.** `tools/names_report.py --export check-chr.jsonl` gives, per language and per names group of the word (`shown` when all its answers carry it, `not_shown`, `mixed`, `unknown`), for all words and for words with `lookalikes` and with `mark_only`: the can't-tell rate, the pairwise agreement, and, where the export says which reading is the reference (`a_is` or `b_is`), the share of the answers that picked a reading which picked the reference one. A row under 20 answers prints "too few answers". Visitors choose the option themselves, so a difference between groups can come from who turns it on as well as from the names.

## Which font each answer was given with

Each answer stores one small value about the page that sent it: whether the font the page serves for the word's script had loaded when the person answered (#1939). The page asks the browser's CSS Font Loading API (`document.fonts.check`) about the family `style.css` declares, for the letters shown, and sends the result as `font` (`fontState` in `public/app.js`). The stored values (`FONT_STATES` in `worker.mjs`, a `CHECK` on the column):

| Value        | Meaning                                                                                                                                                                                               |
| ------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `loaded`     | The word has letters a bundled font covers (today Cherokee, `Noto Sans Cherokee`), and it had loaded.                                                                                                 |
| `not_loaded` | The same, and it had not. The page keeps the answer buttons off in that state, so this value should be rare.                                                                                          |
| `none`       | No bundled font applies to the letters shown (Hebrew letters, Cyrillic): the page draws them with whatever the visitor has.                                                                           |
| `unknown`    | The browser has no font loading API or the check threw; the page is from before the value existed (a cached page sends none); the answer is older than the column; or the request sent anything else. |

Nothing else about the visitor is stored: no font list, no user agent, no screen size. The page does not enumerate fonts. An answer changed later takes the new answer's value. Seed answers are `unknown`. The export gives each word `font_counts` (`loaded`, `not_loaded`, `none`, `unknown`), which add up to the total of `counts`; a word marked `confusable_display` also has `excluded_font_counts` for its set-aside answers, which keep their value in `answers_excluded`. Existing readers that use `counts` and `excluded_counts` are unaffected.

`migrations/0003-answer-font.sql` adds the column to `answers` and `answers_excluded` with `DEFAULT 'unknown'`, so every stored answer reads `unknown`. It only adds: the Worker from before it keeps working on the migrated database, so run it before the merge. The new Worker fails without it. The Worker still accepts answers from a page that sends no `font`.

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

**Blind tasks.** A task body may carry a `private` object. The Worker leaves it out of `/api/next`, and out of `/api/export` until the task has settled; before that the export has the readings, counts and `strata` but not `a_is`, `b_is` or `truth`. A settled task with `private` takes no more answers (`/api/answers` answers 409 and the page moves on to another word), so a revision can't reopen it after its readings were exported. A task with no `private` behaves as before. `en_pick.py` puts in `private`: `a_is` and `b_is` (`xiaomi` or `loc`; which reading is `a` is drawn from a secret key and the task id: `en_pick.py` writes a random `side-key.private` beside its output, which stays on the build host, since task ids are public and `--seed` is documented), `strata` (`decade`, `damage_band`), `crop`, and for a calibration item `truth` (`xiaomi`, `loc` or `neither`: which reading matches the human transcription exactly), `truth_word` and `closer`.

**Making tasks.** `tools/en_pick.py` takes one JSON line per page (LoC's ALTO, the page image, the damage rate, and each crop's box and Xiaomi text; its docstring has the format). It takes LoC's words in each crop, aligns them with Xiaomi's words and keeps the words where they differ, but only when the alignment is certain: the readings agree on most of the crop, the differing stretch has agreeing words on both sides and as many words on each side, and each pair is alike enough. Words it cannot place (at a crop's edge, split, joined or dropped, or too unlike) are counted in `report.json`, never guessed. It then draws up to `--per-page` words per page (one per distinct pair of readings) and at most `--per-stratum` per decade and damage band, with `--seed`, and cuts the word and line images from the page image at LoC's boxes.

**Calibration.** There is no gold-item machinery: the Cherokee sets' seed answers count as one person's answer. A calibration set is ordinary tasks, in the same language and queue so volunteers can't tell them apart, whose `private.truth` comes from a human transcription (en-op, en-as). `tools/en_pick_gold.py` makes the pages file from a gold set directory (crops, manifest, ALTO, Xiaomi readings). Load them as a separate `--set` and never seed answers for them.

```sh
python3 tools/en_pick_gold.py --set-dir sets/en-op --xiaomi xiaomi.jsonl --id-prefix en-op/ --alto-dir loc --out pages-cal.jsonl
python3 tools/en_pick.py --pages pages-cal.jsonl --set cal-v1 --language-config languages/en.json --out-dir out-cal --per-page 20
python3 tools/en_pick.py --pages pages-pilot.jsonl --set xiaomi-loc-v1 --language-config languages/en.json --out-dir out-pilot --per-page 8 --per-stratum 500
```

**Analysis.** `tools/en_pick_analysis.py --export check-en.jsonl --set xiaomi-loc-v1 --calibration-set cal-v1` gives Xiaomi's win rate (Xiaomi picks over Xiaomi plus LoC picks) with a page-clustered bootstrap interval, overall and by decade and damage band, the shares of neither, can't tell, not text and disputed, and for the calibration set how often the settled pick matches the truth. Rows with a `truth` are never in the pilot figures.

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

## Updating loaded tasks

The original build inputs for the Yiddish tasks (the lines file and the line images) are not kept, and they must not be rebuilt by fetching the pages from the Library of Congress again. The Yiddish tasks are therefore updated in place, with `tools/recompute_marks.py`. It works for any loaded language, and it is how the loaded tasks of `yi` (`mark_only`) and `sr` (`lookalikes`) are brought up to the rules of #1939.

Each task body stored in D1 carries both readings. `recompute_marks.py` runs `lookalike.analyse`, the call `build_tasks.py` makes for the two fields, on every body of a language with that language's `equivalents` from `languages/<lang>.json`, and writes new bodies that differ from the old ones only in `mark_only` and `lookalikes` (a field added, changed or removed). It does not touch images, clips or the language configuration, so it does not use `load.sh`, which would also send `language.json` and replace the stored letter pictures.

**Where the bodies come from.** `GET /admin/dump` (what the nightly backup reads) returns every task row, with the whole body. The public `/api/export` leaves out most body fields (the clip keys, `box`, the letter indexes, the labels), so it can only preview. The dump also holds the answers; the tool keeps only the task rows and writes nothing else to disk. `--vault NAME` reads the dump with the admin token from Key Vault (`usnm-prod-check-admin-token`); the token is held in memory and never on a command line. `--dump FILE` takes a saved dump and `--bodies FILE` a JSON-lines file of `{"id", "lang", "set", ...body}` (such as `build_tasks.py`'s `tasks.jsonl`; whole-number floats such as `1.0` are read as `1`, the way the site stores them), so the tool can be run and tested offline.

**Key order.** The stored bodies are in the order `build_tasks.py` writes them (not sorted), and the new bodies keep it. A field the body already has is replaced where it is. A field it gains goes after the last key, `lookalikes` before `mark_only`, where `build_tasks.py` puts them. A field that no longer applies is dropped. The inner keys come from `lookalike.analyse`, in ascending order.

**What the run checks.** For each task the new body must equal the old one in every key but the two, with the same keys in the same order and the same values, and the same `id`, `lang` and `set`. One difference fails the run, and no bodies are written. The run also fails, with no bodies, when:

- this Pillow has no Raqm and the language's readings contain combining marks (those letter pairs can't be drawn the way a browser draws them, so they are refused, not skipped);
- some letter pairs have no reference font or glyph, unless `--allow-unmeasured`;
- a task has no `a` or `b`, or the language has no `languages/<lang>.json`.

Cherokee is measured in the font the site serves (`public/fonts/`); `--font CHEROKEE=PATH` is refused. Other scripts use the reference font that `lookalike.py` finds, or `--font SCRIPT=PATH`. A warning lists tasks whose stored `a_diff` or `b_diff` differ from what the language configuration gives, which points at a set built with other `--equivalent` letters (pass the same `--equivalent` to the tool).

**What `/admin/tasks` keeps.** `POST /admin/tasks` (the `admin` function in `worker.mjs`) inserts each line with `ON CONFLICT (id) DO UPDATE SET body = excluded.body`. For a task that exists it replaces `body` and nothing else: `set_name`, `n`, `outcome`, `settled`, `confusable_display` and `held` are not in the statement, and it does not touch the `answers` or `answers_excluded` tables. So answers, counts, outcomes, settled state, the confusable marks and the set-aside answers stay as they are. A line with `if_body` is a conditional replacement (`UPDATE ... WHERE body = <if_body>`): it never inserts, and a task whose stored body is no longer that text is left alone and returned in `stale`. The tests "/admin/tasks replaces a task's body and leaves its answers..." and "/admin/tasks with if_body..." in `worker.test.mjs` runs it on SQLite with the real schema against a settled task and a marked, held task, and compares every column and both answer tables before and after.

### Steps

1. Dry run. It reads, writes nothing to the site, and writes `OUT/<lang>/tasks.jsonl` (the changed tasks' new bodies), `report.json` and `report.txt`. Use a Pillow with Raqm (`python3 -c "from PIL import features; print(features.check('raqm'))"` prints True; Pillow's pip wheels have it).

   ```sh
   python3 edge/check/tools/recompute_marks.py --lang yi --lang sr --vault usnmtlskvprodffapzy --out-dir OUT
   ```

   To preview from the public exports only (no bodies, no load): `--export check-yi.jsonl --export check-sr.jsonl` in place of `--vault`.

2. Read `OUT/<lang>/report.txt`: tasks read, unchanged and changed, by field, by kind (added, changed, removed) and by mark, and the hashes. `report.json` also lists the changed task ids. Check that the counts are the ones you expect before going on.
3. Load. It reads `report.json` and `tasks.jsonl` and refuses unless the report passed, was made from full bodies, and its hash matches the `tasks.jsonl` on disk. It then reads the site's bodies again and refuses if they are not the ones the dry run read, checks each new body against its current one again, sends the changed tasks to `/admin/tasks` in chunks of 200, reads the bodies back and fails if any differs from what was sent or if any other task changed. Each line carries `if_body`, the stored body text it read, so a task that another loader changed after the read is not overwritten: the reply lists it in `stale` and the run fails.

   ```sh
   python3 edge/check/tools/recompute_marks.py --action load --lang yi --lang sr --vault usnmtlskvprodffapzy --out-dir OUT
   ```

4. Verify with the export. `curl -s 'https://check.usnewsmap.com/api/export?lang=yi' > check-yi.jsonl` has `mark_only` and `lookalikes` on the changed tasks, with the same `counts` and `settled` as before the load; `python3 edge/check/tools/lookalike.py --export check-yi.jsonl` should now report the same totals as the dry run, and a second dry run should report 0 changed. The public export is cached for five minutes.

Run it again after a change to `lookalike.py` or to a language's `equivalents`: the report says what that change did to the loaded tasks before anything is sent.

A dry run that fails for any reason, including an input file that is missing, unreadable or malformed JSON, removes that language's earlier `tasks.jsonl` and `report.txt` and replaces `report.json` with a failed one, so a stale dry run can't be loaded. The load's read-back compares the ids of both snapshots (before and after the send): a task another loader edits, deletes or inserts during the load fails the check.

## Keeping build inputs

From now on, when a language's tasks are built, the inputs are archived before the tasks are loaded, under the lake path `check-site/<lang>/<set>/` on the project's storage host (the host that holds the lake and the scan archive; the build host copies to it). The directory holds:

- `lines.jsonl` and the line images it names (`--images`), exactly as `build_tasks.py` read them;
- `merged-lines.jsonl`, the lines the line tool left out because their ALTO box covers more than one printed line ([above](#words-cut-from-a-merged-line)); it is empty when none were;
- `command.txt`, the exact `build_tasks.py` command line (and the `yiddish_lines.py`, `serbian_lines.py` or `phoenix_lines.py` command before it);
- `tool-commit.txt`, the output of `git rev-parse HEAD` for the checkout that ran them, plus `git status --porcelain` (empty for a clean build);
- `SHA256SUMS` for all of the above.

On the build host, after the build and before loading:

```sh
LAKE=<lake root on the storage host>     # where the lake is mounted or reachable from the build host
SET=$LAKE/check-site/yi/loc-yidj2-v1     # check-site/<lang>/<set>
mkdir -p "$SET"
cp yi/lines.jsonl yi/merged-lines.jsonl "$SET/"   # phoenix_lines.py writes merged-lines.jsonl beside its lines.jsonl, in OUT_DIR/<kind>/
(cd yi && find . -type f \( -name '*.jpg' -o -name '*.png' \) -print0 | tar --null -T - -cf "$SET/images.tar")
printf '%s\n' 'python3 tools/build_tasks.py --lines yi/lines.jsonl ...' > "$SET/command.txt"   # the real command, in full
{ git rev-parse HEAD; git status --porcelain; } > "$SET/tool-commit.txt"
(cd "$SET" && sha256sum * > SHA256SUMS)
```

Nothing in an archive is deleted. A build whose inputs are not archived is not loaded. The Library of Congress files in it were fetched once, by the build, and are not fetched again.

Where the inputs of the sets already loaded should be, and what the repository shows about them:

| Set                                                          | Should be at                                                                                       | What the repository says                                                                                                                                                                                                                                                |
| ------------------------------------------------------------ | -------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `yi` `loc-yidj2-v1`                                          | `check-site/yi/loc-yidj2-v1/`                                                                      | Not kept: the lines file and images are gone. `ocr/bench/yiddish-newspaper/manifest.json` lists the pages, which is not the lines or the images.                                                                                                                        |
| `sr` `loc-boinc-tess-v1`                                     | `check-site/sr/loc-boinc-tess-v1/`                                                                 | Unknown. The build read a pages list, the held jobs' ALTO files and the scans under `/tank/ocr/scans/keep` (see [above](#making-tasks-from-two-readings)); the repository holds none of them and says nowhere that they were archived.                                  |
| `chr` `phoenix-nt-v1`                                        | `check-site/chr/phoenix-nt-v1/`                                                                    | Unknown. The build read `data.json` and wrote `seed.jsonl` on the build host; the repository holds neither.                                                                                                                                                             |
| `chr-lines` `disputes-v1`, `chr-rare` `rare-v1` and `nah-v1` | `check-site/chr-lines/disputes-v1/`, `check-site/chr-rare/rare-v1/`, `check-site/chr-rare/nah-v1/` | Partly. `docs/reports/2026-11-cherokee/` holds `check-lines-manifest.jsonl` and `check-glyph-index.jsonl` (the words and boxes), not the line images or the build's `lines.jsonl`; `phoenix_lines.py` reads the 190 re-read ALTO files and the scans on the build host. |

If a set's inputs turn up, copy them to its path with the command above. Loaded tasks do not depend on them: `recompute_marks.py` needs only the stored bodies.

## Deploying

CI deploys the site: after `ci` passes on main, `.github/workflows/check-site-deploy.yml` runs `edge/check/deploy.sh`, which deploys when `edge/check` differs from what is live. The site serves `/build.txt`, the git tree hash of `edge/check` it was built from, so `curl https://check.usnewsmap.com/build.txt` against `git rev-parse origin/main:edge/check` shows whether main is live. To deploy again without a change, run the workflow by hand with "force". The script also runs from a clean checkout with `az` signed in to the vault (`edge/check/deploy.sh --force`).

Every run, whether it deploys or not, also sends `languages/*.json` to the site with `sync-languages.mjs`, so a merged change to a name, intro or letter note goes live with it. The site's `examples` are kept and the build options are left out, as `from_tap_data.py` does; a language the site doesn't have yet is skipped, since it needs its tasks and clips through `load.sh`. The CI identity reads `usnm-prod-check-admin-token` for this through the grant in `infra/modules/edge-worker-ci.bicep`. To see what a sync would change without sending anything: `node edge/check/sync-languages.mjs --dry-run`.

Changes to an existing database are in `migrations/`, run once in order (`0005-mark-reason.sql` before the Worker that marks with a reason is, and `0004-answer-names.sql` before the Worker that stores `names_shown` is): `0001-answers-notext.sql` must run before a page with "Not text" is deployed, `0002-confusable-display.sql` before the Worker that marks `confusable_display` is, and `0003-answer-font.sql` before the Worker that stores `font` is, `0004-answer-names.sql` before the Worker that stores `names_shown` is, and `0005-mark-reason.sql` before the Worker that stores `mark_reason` is. The D1 schema is applied with `wrangler d1 execute usnm-check --remote --file schema.sql`, or through the D1 API; every statement is idempotent.

### Secrets

`SESSION_KEY` signs sessions, `ADMIN_TOKEN` guards `/admin/*`, and `TURNSTILE_SECRET` verifies the browser check. `set-secrets.sh` keeps the first two in Key Vault (`usnm-prod-check-session-key`, `usnm-prod-check-admin-token`) and sets all three on the Worker:

```sh
edge/check/set-secrets.sh --vault usnmtlskvprodffapzy --token-secret usnm-prod-workers-ci-token \
  --account-id <account>
```

Changing `SESSION_KEY` (`--rotate`) signs everyone out; their earlier answers stay.

## Tests

```sh
node --test edge/check/worker.test.mjs edge/check/layout.test.mjs edge/check/languages.test.mjs edge/check/fonts.test.mjs
```

The `check site` job in `ci.yml` runs these, a Wrangler dry-run build (`npx wrangler@4.154.0 deploy --dry-run`), shellcheck of the scripts and the tools' unit tests whenever anything under `edge/check/` but its Markdown changes. `languages.test.mjs` checks each language configuration's fields and that its keys are sorted. `fonts.test.mjs` checks the served font against the hash in `public/fonts/README.md`. `worker.test.mjs` runs the marking and reopening on SQLite with the real schema. `tools/test_cherokee_labels.py` checks the Cherokee names against the Unicode names and that the three files agree. `tools/test_names_report.py` runs the names report on a synthetic export. `layout.test.mjs` renders names on and off in headless Chrome at 375px, left to right and right to left. `tools/test_mark_report.py` runs the mark report on a small synthetic export. `tools/test_merged_lines.py` and `tools/test_merged_lines_loaded.py` test the rule, the line tools' use of it (they need Pillow) and the loaded-task tool on small `tar.bz2` archives built in the tests. `tools/test_recompute_marks.py` runs `recompute_marks.py` on small synthetic bodies and against a local stand-in for the two admin routes (it needs Pillow with Raqm, and skips otherwise). `tools/test_lookalike.py` holds the table of letter pairs the threshold comes from and needs Pillow with a FreeType that reads woff2 (it skips otherwise).

`layout.test.mjs` renders every language's sample words in headless Chrome, one highlighted letter at a time, and fails if a highlight box overlaps another character. It skips when no Chrome is found (set `CHROME=/path/to/chrome`) unless `CI` is set.
