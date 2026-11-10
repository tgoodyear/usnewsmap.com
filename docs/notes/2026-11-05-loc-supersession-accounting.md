# Avoided-work accounting for a supersession list

Follow-up to [pinned supersession](2026-11-05-loc-alternate-supersession.md),
for [#811](https://github.com/tgoodyear/usnewsmap.com/issues/811). This
reports what a stored list would avoid. It is read-only and changes no
scheduling.

**Outcome:** merged in PR #1577 on 5 November 2026.

## Command

`usnm-ingest ocr-alternate-accounting --supersession <sha256>` loads the
stored list (the same hash and format checks as the seeder) and the OCR
ledger. It places each listed page in the seeder's own order: ledger skips
come first, then the list.

| Standing     | Meaning                                                                                                            |
| ------------ | ------------------------------------------------------------------------------------------------------------------ |
| `imported`   | Already recognised and imported: spent, not avoided                                                                |
| `in_flight`  | Queued with no result: never cancelled, so not avoided                                                             |
| `settled`    | Queued and settled without an import: only retries and `ocr-enqueue`, which the list doesn't touch, queue it again |
| `avoidable`  | Never queued, and the exact selection verifies now: left out if a pinned plan reaches it                           |
| `unverified` | Never queued, and the selection doesn't verify: queued normally                                                    |

Only never-queued pages are verified, as in the seeder. The JSON output
gives each count, each page's standing, `recognition_cost: "unknown"` and the
report's scope. Whether a plan reaches a page is a seed dry run's
`supersession` report, which the run also writes to `status/ocr-local.json`.
Accounting needs no setting, so a list can be measured before anyone pins it.

## Evidence

A synthetic fixture stores five verified selections, then:

- queues three of the pages, imports one and dead-letters another;
- revokes the fifth page's selection.

Accounting places one page in each standing and writes nothing. A pinned
seed dry run on the same stores agrees: it leaves out the avoidable page and
counts the unverified one.

Production has no stored list, selection or staged alternate, by the
read-only listing in the
[supersession note](2026-11-05-loc-alternate-supersession.md). So the command
has nothing to account for there, and confirmed avoidable pages remain 0.
Recognition cost per page is not measured, so avoided cost is reported as
unknown, never as zero or as an estimate.
