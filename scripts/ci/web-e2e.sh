#!/usr/bin/env bash
# Build the site and run the Playwright + axe suite against the API started by
# start-api-fixtures.sh, which serves the build as production does. The
# tests intercept the default basemap style and serve a local one, so no
# third-party tile service is needed.
set -euo pipefail
cd web
npm ci
npx playwright install --with-deps chromium
npm run build
node scripts/precompress.mjs dist
PW_BASE_URL=http://127.0.0.1:8080 npm run e2e
