#!/usr/bin/env bash
# Build the site for an environment; VITE_API_BASE is its API origin.
set -euo pipefail
if [ -z "${VITE_API_BASE:-}" ]; then
  echo "::error::USNM_API_URL is not set on this environment (run scripts/bootstrap.sh)"
  exit 1
fi
cd web
npm ci
npm run build
