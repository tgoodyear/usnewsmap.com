#!/usr/bin/env bash
# Build report.pdf: SVG figures, then Markdown → HTML (pandoc) → PDF (Chromium via Playwright).
# Needs python3, pandoc and the web app's node_modules (for Playwright).
set -euo pipefail
cd "$(dirname "$0")"
python3 figures/make_figures.py
pandoc report.md --standalone --embed-resources --css style.css --metadata pagetitle="Languages in the usnewsmap index: status, October 2026" -o report.html
WEB="$(git rev-parse --show-toplevel)/web"
NODE_PATH="$WEB/node_modules" node --input-type=module -e "
import { chromium } from '$WEB/node_modules/playwright/index.mjs';
const b = await chromium.launch();
const p = await b.newPage();
await p.goto('file://$PWD/report.html');
await p.pdf({ path: 'report.pdf', format: 'Letter', margin: { top: '0.7in', bottom: '0.7in', left: '0.75in', right: '0.75in' }, printBackground: true });
await b.close();
"
echo "wrote report.pdf"
