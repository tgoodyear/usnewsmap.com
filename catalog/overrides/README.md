# Catalog overrides

Both files are compiled into `usnm-ingest`, so a correction ships with the
next image and applies on the next ingest run, in every environment (08
§8.9: hand-curated state lives in git).

## Place overrides

`places.json` corrects the coordinates `geocode` would otherwise take from
LoC's title records (or a state centroid).

Each entry names a place by its city and state, as the catalog shows it:

```json
[
  { "city": "Lusk", "state": "WY", "lat": 42.7625, "lon": -104.4522 },
  { "city": "Honolulu", "state": "HI", "lat": 21.3069, "lon": -157.8583, "precision": "city" }
]
```

`precision` is `city` (the default), `county` or `state`.

Current entries:

- **Vancouver, WA:** for The Vancouver Independent (pages `sn87093109`, record `sn84022797`, #120). Neither of its LoC records has coordinates, so without this it would sit at Washington's centroid. The coordinates are the city's, from public gazetteers, not from LoC.

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
