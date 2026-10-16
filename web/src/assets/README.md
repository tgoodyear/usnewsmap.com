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

The script verifies the source SHA256, uses the lockfile-pinned mapshaper,
retains 3% of vertices with `keep-shapes`, rounds to 0.001 degrees, sorts by
state code, and retains only `STUSPS` and `NAME`. It verifies the 56 features
and a maximum gzip size of 100,000 bytes. The generated asset measures
**16,221 bytes gzip** (`node:zlib`, level 9). Vite emits a content-hashed
`/assets/states-*.geojson` URL, fetched only when States is first drawn and
cached immutably by the API. No Azure resource or external runtime request
is needed.
