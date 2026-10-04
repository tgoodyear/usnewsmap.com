#!/usr/bin/env bash
# The Playwright + axe suite against the API started by start-api-fixtures.sh,
# which serves the build as production does. The tests intercept the default
# basemap style and serve a local one, so no third-party tile service is
# needed.
#
#   web-e2e.sh build   install, build the site and precompress it
#   web-e2e.sh test    run the suite (the API must be up)
#
# CI runs them as separate steps so the API's build (started in the
# background beforehand) overlaps the site's; with no argument, both run.
set -euo pipefail
cd web
step=${1:-all}
case "$step" in
  build | test | all) ;;
  *) echo "usage: web-e2e.sh [build|test]" >&2; exit 2 ;;
esac
if [ "$step" = build ] || [ "$step" = all ]; then
  npm ci
  npx playwright install --with-deps chromium
  npm run build
  node scripts/precompress.mjs dist
fi
if [ "$step" = test ] || [ "$step" = all ]; then
  PW_BASE_URL=http://127.0.0.1:8080 npm run e2e
fi
