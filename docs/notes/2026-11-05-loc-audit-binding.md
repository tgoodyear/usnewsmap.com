# Binding LoC crop scores to the audited package

For [#811](https://github.com/tgoodyear/usnewsmap.com/issues/811), after the
[package audit](2026-10-27-loc-package-audit.md). This ties the quality adapter's
evidence to the audited package's bytes. It measures no accuracy, and it makes
no new request to the Library of Congress.

## Change

`loc_align.py` takes an optional `--audit-inventory`, which is the per-page
`loc_archive.py --out` inventory (`usnm-loc-ocr-artifacts/1`), with its pinned
`--audit-inventory-sha256`. When it's given, each binding must:

- name an NDNP `www.loc.gov/resource/...?sp=N` page URL, read as the canonical
  page ID with the same rule as `loc_inventory.py`;
- find that page in the inventory;
- carry the `alto_sha256` of exactly that page's audited ALTO member.

Anything else is an `error` row and a nonzero exit, as for other binding faults.
An aligned row records the page ID, archive and inventory hashes, the member
path and size, and the ALTO's embedded software. The report records both
hashes, and its limitations say the audit binds bytes to a page ID, not to the
source scan. Without the option, the reports are unchanged. The inventory is
read with the same size bound and checksum check as the other inputs. Its
format, archive hash, page count and page-ID uniqueness are checked too.

## Measured overlap

These are read-only counts over published GitHub release assets only. Each was
checked against its release's `SHA256SUMS`. The audited inventory is
[`fu-three-inventory.json`](https://github.com/tgoodyear/usnewsmap.com/releases/tag/data-loc-package-audit-20261027),
SHA-256 `f9caf44ce4a509e96f843fdba9e52c9dd7266a54468751d5cb8adf8eceef9ab8`, with
1,706 pages and 1,706 distinct page IDs.

| Benchmark manifest | Samples | NDNP pages | In the audited package |
| ------------------ | ------: | ---------: | ---------------------: |
| `ocr-bench-v1.0.0` |   1,000 |        300 |                      0 |
| `ocr-bench-v1.1.0` |      52 |          0 |                      0 |

All 1,052 samples are human-verified, but none is on an audited page. So no
paired score of the audited package's OCR is possible from current data. That
is the human-reference common set [#810](https://github.com/tgoodyear/usnewsmap.com/issues/810)
must supply. The adapter only makes such a score traceable to the package once
it exists. Rights stay as the audit's `rights-and-history.json` records them:
there is no separate OCR, ALTO or layout licence.
