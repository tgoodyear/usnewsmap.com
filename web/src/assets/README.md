# State boundaries

`states.geojson` is derived from the US Census Bureau **2024 cartographic
boundary file, states at 1:5,000,000**:
<https://www2.census.gov/geo/tiger/GENZ2024/shp/cb_2024_us_state_5m.zip>.
It is a public-domain US federal government work, not an OpenStreetMap derivative.
It includes 50 states, DC, Puerto Rico, Guam, American Samoa, the Northern Mariana
Islands and the US Virgin Islands, in their true geographic positions.

Rebuild from the repository's `web/` directory after `npm ci`:

```sh
mkdir -p scripts/data
curl --fail --location \
  --output scripts/data/cb_2024_us_state_5m.zip \
  https://www2.census.gov/geo/tiger/GENZ2024/shp/cb_2024_us_state_5m.zip
node scripts/prepare-states.mjs \
  --input scripts/data/cb_2024_us_state_5m.zip \
  --output src/assets/states.geojson \
  --names src/lib/stateNames.json
```

The script verifies the source's SHA256, uses the mapshaper pinned in the
lockfile, keeps 3% of the vertices with `keep-shapes`, rounds to 0.001 degrees,
sorts by state code and keeps only `STUSPS` and `NAME`. It checks that there
are 56 features and that the gzip size is at most 100,000 bytes. The generated
asset is **16,221 bytes** gzipped (`node:zlib`, level 9). Vite emits it at a
content-hashed `/assets/states-*.geojson` URL; it is fetched only when States
is first drawn, and the API serves it as immutable. It needs no Azure resource
and no request to another service at run time.
