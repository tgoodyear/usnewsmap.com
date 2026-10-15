# Engine catalog and credits schema (schema = 1)

`engines.toml` records the licence decisions in design 13 §13.7.2 and
ADR-0016 decision 8. `usnm-ocr-route::Catalog::load` reads it; the worker embeds
a copy so installations need no source checkout. An explicit `--engines` file
replaces, rather than supplements, that copy. Unknown schemas, duplicate ids
or names, empty profiles, and dangling model references fail loading.

The inventory includes checked engines/models not yet implemented. Only
Tesseract has production profiles here. `production = false` keeps every
other engine unroutable; recording a cleared model does not deploy it. Code
not separately checked in the design stays `unknown`, not implicitly Apache.
No tier-2 or credit-required profiles are added before the credits page (#372).

## Shape for #372

- `production_licenses`: array of licence identifier strings approved by the
  maintainer. `credit_required`: array of licences whose terms mandate credit.
- `[[engine]]`: `id` (unique), `family`, `production` (defaults to false), the
  code's attribution fields below, and nested `model` and `profile` arrays.
- `[[engine.model]]`: `name` (unique within its engine, e.g.
  `tessdata/eng.traineddata`) plus the same attribution fields as the code.
- `[[engine.profile]]`: `id` (unique within its engine), nonempty `models`
  (exact model names), and nonempty `languages` (ISO 639-3 strings). File order
  is the preference order for the current minimal Tesseract route.

Attribution fields are direct TOML keys, **not embedded JSON, Markdown or HTML**:

| Key | TOML type | Meaning |
| --- | --- | --- |
| `license` | string | SPDX id, project `LicenseRef-*`, `none`, or `unknown` |
| `author` | string | Author/publisher, as named by the source |
| `source` | string | Source URL; for attributed weights, the download source |
| `credit` | string | Plain-text, human-readable credit line (render as text) |
| `fetched` | local date (`YYYY-MM-DD`, unquoted) | Recorded source/fetch date from the design's checked snapshot |
| `note` | string | Evidence, including absence of a licence and where checked for `none` |
| `terms` | string | Licence-text URL; required for cleared `LicenseRef-*` entries |
| `restricted` | optional string | Quoted restrictive clause and its source link; presence always vetoes production |
| `undecided` | optional string | Open maintainer decision and terms link; presence always vetoes production |

For the credits page, identify code by `engine.id` and weights by
`(engine.id, model.name)`. Read each entry's `credit`, `author`, `source` and
`license` independently; **do not parse the credit line to recover metadata**.
Render `none` as “no explicit licence”; link SPDX ids to `spdx.org/licenses/`
and `LicenseRef-*` ids through `terms`. Mark `credit_required` licences as
mandatory attribution. Escape all strings when rendering HTML. Engine
`production` is an eligibility flag, not evidence that every inventoried model
is used: select model credits through the production profiles/routes (and later
the release's usage inventory). The current minimal route names all Tesseract
profiles via `auto`; unused tier-2 Tesseract entries are inventory only.

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
from a model's training data or from its engine's code licence. Future engine
build/runtime limits can coexist as additional engine/profile keys; they do
not change this gate.

`usnm-ocr-worker routes check [--engines PATH] [--routes PATH]` validates the
licences of every candidate and cascade target in the route file; `auto`
checks every profile of that engine. The default route file is
`ocr/routing/routes.toml`. Full matching and node-capability/lock validation
are later design steps, not claimed by this command yet.

Tesseract modes `--psm 0`, `1` and `12` implicitly read `osd.traineddata`.
Both the direct worker and text adapter refuse these modes until OSD is
cataloged, gated and included in provenance. The default mode 3 is unchanged.
