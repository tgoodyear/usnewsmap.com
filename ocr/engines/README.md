# Engine catalog and credits schema (schema = 1)

`engines.toml` records the licence decisions in design 13 §13.7.2 and
ADR-0016 decision 8. `usnm-ocr-route::Catalog::load` reads it; the worker embeds
a copy so installations need no source checkout. An explicit `--engines` file
replaces, rather than supplements, that copy. Unknown schemas, duplicate ids
or names, empty profiles, and dangling model references fail loading.

The inventory includes checked engines and models that aren't implemented yet.
Only Tesseract and the pinned CPU NDLOCR-Lite adapter have production profiles
here. `production = false` keeps the other engines unroutable, and recording a
cleared model does not deploy it. Code the design didn't check separately stays
`unknown`; it is not assumed to be Apache. No tier-2 or credit-required profiles
are added before the credits page (#372).

Tesseract image builds download the unchanged `tesseract/models.lock` URLs
with certificate verification and verify each SHA-256 before accepting it.
Downloads allow 30 retries, including TLS and connection failures: 30-second
connect timeout, 600-second attempt timeout, 900-second retry-start budget.
An in-progress final attempt can extend the total to 1500 seconds per model.
Exhausted retries or checksum mismatches fail the build without alternate
endpoints or unverified models.

There are three `models.lock` files, and each stays with what it pins.
`tesseract/models.lock` is embedded by `usnm-ocr-worker` and read by the
Tesseract image build. `ndlocr-lite/models.lock` is embedded by
`usnm-ocr-adapter` and read by the NDLOCR-Lite image build; it also holds the
commit that engine is pinned at. `scripts/ocr-survey/models.lock` records the
files the engine survey ran (Tesseract, docTR, NDNP and Surya models among
them) and feeds only the survey's own images, through `fetch-models.sh`; it
repeats the Tesseract entries and Surya's weights as they were run. The pins an
engine needs in more than one place are checked by
`scripts/ci/check-engine-pins.py`.

## Credits data shape

- `production_licenses`: array of licence identifier strings approved by the
  maintainer. `credit_required`: array of licences whose terms mandate credit.
- `[[engine]]`: `id` (unique), `family`, `production` (defaults to false), the
  code's attribution fields below, and nested `model` and `profile` arrays.
- `[[engine.model]]`: `name` (unique within its engine, e.g.
  `tessdata/eng.traineddata`), optional `version` (published revision), plus the
  same attribution fields as the code. A pinned image model's version must
  match its engine image's `models.lock` (`tesseract/` or `ndlocr-lite/`); the
  web catalog test checks them. These pins don't prove which revision read a
  served page: short engine keys omit model digests, so release credits and the
  web's legacy fallbacks leave those model revisions unrecorded rather than
  labelling old text with today's pin.
- `[[engine.profile]]`: `id` (unique within its engine), nonempty `models`
  (exact model names), and nonempty `languages` (ISO 639-3 strings). File order
  is the preference order for the current minimal Tesseract route.

Attribution fields are direct TOML keys, **not embedded JSON, Markdown or HTML**:

| Key          | TOML type                           | Meaning                                                                          |
| ------------ | ----------------------------------- | -------------------------------------------------------------------------------- |
| `license`    | string                              | SPDX id, project `LicenseRef-*`, `none`, or `unknown`                            |
| `author`     | string                              | Author or publisher, as named by the source                                      |
| `source`     | string                              | Source URL; for attributed weights, the download source                          |
| `credit`     | string                              | Plain-text, human-readable credit line (render as text)                          |
| `fetched`    | local date (`YYYY-MM-DD`, unquoted) | The source or fetch date recorded in the design's checked snapshot               |
| `note`       | string                              | Evidence, including absence of a licence and where checked for `none`            |
| `terms`      | string                              | Licence-text URL; required for cleared `LicenseRef-*` entries                    |
| `restricted` | optional string                     | Quoted restrictive clause and its source link; presence always vetoes production |
| `undecided`  | optional string                     | Open maintainer decision and terms link; presence always vetoes production       |

The web build parses this TOML directly (Vite's `ocr-credit-catalog` plugin);
`web/src/credits/data.ts` selects the serving engines and profile models, and
the unit and end-to-end tests reject missing credits. No generated list is
maintained.

For the credits page, identify code by `engine.id` and weights by
`(engine.id, model.name)`. Read each entry's `credit`, `author`, `source` and
`license` independently; **do not parse the credit line to recover metadata**.
Render `none` as “no explicit licence”; link SPDX ids to `spdx.org/licenses/`
and `LicenseRef-*` ids through `terms`. Mark `credit_required` licences as
mandatory attribution. Escape all strings when rendering HTML. Engine
`production` is an eligibility flag, not evidence that every inventoried model
is used. Select model credits through the production profiles and routes (and
later the release's usage inventory). The current minimal route names all
Tesseract profiles via `auto`; unused tier-2 Tesseract entries are inventory
only.

## The shared gate

`Catalog::check_production(engine_id, complete_profile_model_names)` is the
single gate used by both worker implementations and `routes check`:

1. The engine must be recorded and `production = true`.
2. Code **and every model actually read** need nonblank `author`, `source`,
   `credit`, and either a licence in `production_licenses`, or `license = "none"`
   with a nonblank `note` and a `fetched` date.
3. Any `restricted` or `undecided` key (even an empty value), `unknown`, missing
   licence, or non-allowlisted licence refuses the entry. Adding `none` or
   `unknown` to the allowlist cannot bypass their rules.
4. Custom cleared licences also need nonblank `terms` for the credits link.

Only the maintainer changes tier decisions. The catalog does not infer terms
from a model's training data or from its engine's code licence. Future limits
on engine builds or runtimes can be added as further engine or profile keys;
they do not change this gate.

`usnm-ocr-worker routes check [--engines PATH] [--routes PATH]` validates the
licences of every candidate and cascade target in the route file; `auto`
checks every profile of that engine. The default route file is
`ocr/routing/routes.toml`. Full matching and the validation of node
capabilities and locks are later design steps; this command doesn't do them yet.

Tesseract modes `--psm 0`, `1` and `12` implicitly read `osd.traineddata`.
Both the direct worker and text adapter refuse these modes until OSD is
cataloged, gated and included in provenance. The default mode 3 is unchanged.

A new release also freezes the entries of the engine used and the models
selected in `credits.json`, using `usnm-ocr-route::Catalog::credits`. Unknown
keys block publication. The API verifies the file against the reference
manifest and returns it in `published.sources.ocr_credits`; the web page
prefers that copy over its build-time fallback for older releases. The Japanese
per-engine counts cover the union of the serving Japanese search index and the
main index's Latin replacements, deduplicated per page and engine, and are
frozen with the credits. They do not come from the current OCR job.

## Tesseract contract output

`usnm-ocr-adapter-tesseract-text --contract 1 --alto` advertises word-level
ALTO and invokes Tesseract's ALTO renderer. It validates that XML with
`usnm-alto` and derives the accompanying text from the same layout; malformed
output fails the page. Request DPI, profiles, model hashes, thread limits and
process-group deadline cancellation work as in the default text mode.
Without `--alto`, the existing page-sized text-box output is unchanged.

This opt-in output mode does not upgrade the pinned Tesseract image, install
an adapter in that image, or switch production workers to the engine contract.
Those release and rollout requirements remain separate in #307.
