# Catalog overrides

These files are compiled into `usnm-ingest`, so a correction ships with the
next image and applies on the next ingest run, in every environment (08
§8.9: hand-curated state lives in git).

## Audit trail

Every entry, in every file here, records when, why and from what source it
was decided:

- `added`: the date it was decided, `YYYY-MM-DD`.
- `reason`: why. For a place or title override, the titles it is for and
  what `geocode` would do without it.
- `source`: where the new coordinates, name or record come from: a URL, or a
  precise citation such as a LoC title record's LCCN and field, a row of the
  Census gazetteer, or a masthead with its date. For an alias, what shows
  the two names are one town. When the source of an older entry was never
  recorded, `source` says so ("Not recorded: added in PR #190, which gives
  no source") so it can be checked again.
- `ref`: the number of the GitHub issue or pull request where it was
  decided.

When an entry changes later, it gets `updated` (the date of the change) and
its earlier versions go in `history`, oldest first, so the file itself is
the trail. Each item of `history` has the values that changed, as they were
then, with that version's `reason`, `ref`, `source` and `updated` (the date
that version was made; for the first one, the entry's `added`):

```json
{ "added": "2026-10-05", "city": "Lusk", "history": [{ "lat": 42.76, "lon": -104.45, "reason": "Why then.", "ref": 101, "source": "Where from then.", "updated": "2026-10-05" }], "lat": 42.7625, "lon": -104.4522, "reason": "Why now.", "ref": 102, "source": "Where from now.", "state": "WY", "updated": "2026-10-09" }
```

`geocode` and `titles-sync` refuse an entry without these fields, with a
date that isn't `YYYY-MM-DD`, with a `ref` that isn't a positive number, or
with a `history` value that the entry's field couldn't hold (a latitude
that isn't a number, an unknown state),
and the ingest tests also refuse entries or keys out of order (in every
object, `history` items included).

## Place overrides

`places.json` corrects the coordinates `geocode` would otherwise take from
LoC's title records, the gazetteer or a state centroid (04 §4.6), and can
rename the place.

Each entry names a place by its city and state, as the catalog shows it
(spelling variants such as "St."/"Saint" match). Entries are sorted by
state, then city, with keys in alphabetical order:

```json
[
  { "added": "2026-10-07", "city": "Honolulu", "lat": 21.3069, "lon": -157.8583, "name": "Honolulu", "precision": "city", "reason": "Why.", "ref": 123, "source": "Where the coordinates come from.", "state": "HI" }
]
```

`name` replaces the place's name. `precision` is `city` (the default),
`county` or `state`. `history` items can hold `lat`, `lon`, `name` and
`precision`; `"name": null` records that an earlier version had no name.

## Place aliases

`place-aliases.json` names misspelled and renamed towns, so their titles
share a place with the town's other titles, under the town's name:

```json
[
  { "added": "2026-10-06", "from": "Skaguay", "reason": "old spelling", "ref": 190, "source": "What shows the two names are one town.", "state": "AK", "to": "Skagway" }
]
```

`from` is the city as LoC spells it (spelling variants match); `to` is
the town's name, and becomes the place's name. Entries are sorted by
state, then `from`, with keys in alphabetical order. A `to` can't be
another alias's `from`. Only merge towns that are one point on the map: a
town that was separate (East Providence, Winston before 1913) keeps its
own place. For a title naming two cities ("Fort Worth-Dallas"), alias it
to the first. `history` items can hold `to`.

The gazetteer `geocode` uses is `../gazetteer/us-places.csv`, built by
`scripts/build-gazetteer.py` from the Census Bureau's national places
file (public domain).

Spellings that key alike ("St. Paul" and "Saint Paul") and "X City" next
to "X" merge by rule, not by an entry here (04 §4.6). The catalog records
those merges: each place in `catalog/places.json` lists the other names its
titles use and the rule that put each there (`variants`), and the rule its
coordinates came from (`coordinates_from`).

## Title places

`title-places.json` places a title whose LoC record lists several places
(a paper that moved) in one city and state, whatever its record says.
Entries are sorted by `lccn`, with keys in alphabetical order:

```json
[
  { "added": "2026-10-06", "city": "Chicago", "lccn": "sn84024055", "reason": "Where and when it was published.", "ref": 190, "source": "Where that comes from.", "state": "IL" }
]
```

`history` items can hold `city` and `state`.

## Title overrides

`titles.json` is for a title whose pages ship under an LCCN that has no
record on loc.gov (`https://www.loc.gov/item/{lccn}/?fo=json` returns 404),
while another LoC record describes the same title. `titles-sync` then
fetches that record instead and catalogs it under the pages' LCCN, so the
title's batches can be released. The catalog entry keeps the record's LCCN
as `loc_record`.

Entries are keyed by the pages' LCCN, in ascending order:

```json
{
  "sn87093109": {
    "added": "2026-10-05",
    "reason": "Why.",
    "record": "sn84022797",
    "ref": 166,
    "source": "Where the record was found."
  }
}
```

Only name a record that is about the same title: for example one whose
`digital_id` is `chroniclingamerica.loc.gov/lccn/{lccn}/issues` for the
pages' LCCN, and whose first and last issues match the batches' issues.
`titles-sync` doesn't fetch a title that is already in the catalog, so an
entry for such a title takes effect after
`titles-sync --refresh --lccns {lccn}`. `history` items can hold `record`.
