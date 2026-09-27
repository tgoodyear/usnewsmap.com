# Place overrides

`places.json` corrects the coordinates `geocode` would otherwise take from
LoC's title records (or a state centroid). It is compiled into
`usnm-ingest`, so a correction ships with the next image and applies on the
next ingest run, in every environment (08 §8.9: hand-curated state lives in
git).

Each entry names a place by its city and state, as the catalog shows it:

```json
[
  { "city": "Lusk", "state": "WY", "lat": 42.7625, "lon": -104.4522 },
  { "city": "Honolulu", "state": "HI", "lat": 21.3069, "lon": -157.8583, "precision": "city" }
]
```

`precision` is `city` (the default), `county` or `state`.
