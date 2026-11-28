# #968 metadata A/B: frozen template, fields and harness

Frozen before any reading, per the plan and amendment on #968 (28 Nov 2026).

## Template

- Day known: `This text is from <title>, <place>, printed on <date>.`, with the date as, for example, `31 December 1849`.
- Year only: `This text is from <title>, <place>, printed in <year>.`

The metadata arm prepends the sentence and one space to the production prompt of the request version. That's after the image for Qwen 8B (version 3) and before it for Xiaomi-OCR-0 (version 5). The baseline arm sends the production prompt unchanged.

## Fields

`fields.jsonl` has one row per #968 crop, with `title`, `place` and `date`, each beside its `*_source`, plus `date_precision`, `template` and `sentence`. `prefixes.json` maps each included crop id to its sentence: it's the file the harness reads. `build_fields.py` builds both from the sets and the LoC title catalogue.

| Set | Crops | Title, place | Date |
|---|---|---|---|
| `de-nzz` | 360 | the dataset's: Neue Zürcher Zeitung, Zürich | day, from the file name |
| `de-rz81` | 322 | the dataset's: Deutscher Reichsanzeiger, Berlin | year only, from the file name (year_issue_page) |
| `en-op` (descriptive only) | 345 | the LoC title catalogue (`name`, `place_of_publication`) | day, from the crop record |
| NewsEye `fi` and `sv` (`fi-52`, `sv-52`, `new-newseye-nlf-fi`, `new-newseye-nlf-sv`) | 148 left out | known only by ISSN; the one lookup failed on 28 Nov 2026 (the ISSN portal returned a bot-protection page; the NLF serial URL returned 404 and then no response) | |
| NewsEye BnF (`fr-52`, `new-newseye-bnf`) | 84 left out | none in the dataset's metadata | none |

1,027 crops are read in both arms; 232 are left out. The independent sets in the decision are `de-nzz` and `de-rz81`.

## Harness

`usnm_qwen` 1.22's source (`6e51a3a4`, the release's `source_commit`) with one change: when `USNM_QWEN_AB_PROMPT_PREFIXES` names a JSON object of crop id to sentence, that crop's prompt text gets the sentence and a space in front (`request_json_with`). The run's `diag.txt` then records that the harness build was used, with the prefix file's sha256. If the file can't be read, the unit fails. Every other path is the app's: llama-server flags, model files, request settings, loop guard and parsing. Both arms run this build with 1.22's own `llama-server` on CT 214 in one session: the baseline without the variable, the metadata arm with it. This is a harness, not the released app: a passing result goes through an app release and a confirmation run before production.
