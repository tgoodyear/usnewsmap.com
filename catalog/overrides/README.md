# Catalog overrides

Both files are compiled into `usnm-ingest`, so a correction ships with the
next image and applies on the next ingest run, in every environment (08
§8.9: hand-curated state lives in git).

## Place overrides

`places.json` corrects the coordinates `geocode` would otherwise take from
LoC's title records, the gazetteer or a state centroid (04 §4.6), and can
rename the place.

Each entry names a place by its city and state, as the catalog shows it
(spelling variants such as "St."/"Saint" match), with its keys in
alphabetical order:

```json
[
  { "city": "Lusk", "lat": 42.7625, "lon": -104.4522, "state": "WY" },
  { "city": "Honolulu", "lat": 21.3069, "lon": -157.8583, "name": "Honolulu", "precision": "city", "state": "HI" }
]
```

`name` replaces the place's name. `precision` is `city` (the default),
`county` or `state`.

Current entries:

- **Anacostia, DC:** for The Weekly News (`sn82016441`). Anacostia is part of Washington, so the gazetteer has no point for it; it stays its own place, at Anacostia rather than central Washington.
- **Vancouver, WA:** for The Vancouver Independent (pages `sn87093109`, record `sn84022797`, #120). Neither of its LoC records has coordinates, so without this it would sit at Washington's centroid. The coordinates are the city's, from public gazetteers, not from LoC.

## Place aliases

`place-aliases.json` names misspelled and renamed towns, so their titles
share a place with the town's other titles, under the town's name:

```json
[
  { "from": "Skaguay", "note": "old spelling", "state": "AK", "to": "Skagway" }
]
```

`from` is the city as LoC spells it (spelling variants match); `to` is
the town's name, and becomes the place's name. Entries are sorted by
state, then `from`, with keys in alphabetical order. A `to` can't be
another alias's `from`. Only merge towns that are one point on the map: a
town that was separate (East Providence, Winston before 1913) keeps its
own place. For a title naming two cities ("Fort Worth-Dallas"), alias it
to the first.

The gazetteer `geocode` uses is `../gazetteer/us-places.csv`, built by
`scripts/build-gazetteer.py` from the Census Bureau's national places
file (public domain).

## Title places

`title-places.json` places a title whose LoC record lists several places
(a paper that moved) in one city and state, whatever its record says.
Entries are sorted by `lccn`, with keys in alphabetical order:

```json
[
  { "city": "Chicago", "lccn": "sn84024055", "note": "Where and when it was published.", "state": "IL" }
]
```

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
    "note": "Why, and where the record was found.",
    "record": "sn84022797"
  }
}
```

Only name a record that is about the same title: for example one whose
`digital_id` is `chroniclingamerica.loc.gov/lccn/{lccn}/issues` for the
pages' LCCN, and whose first and last issues match the batches' issues.
`titles-sync` doesn't fetch a title that is already in the catalog, so an
entry for such a title takes effect after
`titles-sync --refresh --lccns {lccn}`.
