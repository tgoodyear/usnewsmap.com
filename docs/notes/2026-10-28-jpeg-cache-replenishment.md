# Bounded JPEG-cache replenishment, before deployment

For [#911](https://github.com/tgoodyear/usnewsmap.com/issues/911).
This is implemented and exercised offline, **not installed or accepted on the
two retained converters**. The enabled, exhausted remote converter on the
pve-laptop host is not the active CT215 quality follower. That follower's full
index/set reloads do not satisfy this issue's corpus-scale bound.

## Implementation

[`quality-jpeg-refresh.py`](../../scripts/ocr-node/quality-jpeg-refresh.py)
maintains a SQLite pending queue and recipe-specific completion index. Each
invocation consumes at most `--rows-per-tick` new complete lines from each of
the two completion ledgers and the fetcher's local append-only scan index.
The offset and row updates commit together. Initial catch-up can take multiple
ticks; no list is published until **both** completion cursors catch up.

There is no archive `find`, corpus-sized Python set or full index download.
Only indexed scans that currently exist under local `keep/` enter the queue;
buffer scans are not converted or deleted. JP2 candidates are keyed by path;
JPEG-only batches still retain their place in the damage batch order.
The manifest is aggregated once per file identity, with DuckDB limited to
64 MiB and one thread. Its bounded batch metadata and exact page membership
are cached on disk; an unchanged manifest is not rescanned on the next tick.
Manifest batches admit only their listed page paths, not every cached page
in the same batch. A changed manifest requires a one-time streamed membership
rebuild and eligibility reconciliation; that bootstrap's work is proportional
to the input, unlike ordinary cursor ticks.

The existing list builder's `resolve_current` / `remaining_batches` helpers
retain manifest shard assignment and conservative tier-1 drift coverage.
Indexed queries produce a bounded interleaved candidate window in document-ID
order. Low/high page watermarks limit each converter's owned list. Ownership
commits before either atomic list replacement, so a failed publication can
resume without assigning the same page to both hosts. A process lock refuses
overlapping refreshes.

The converter gains opt-in `--keyed-completions --follow-list`. This
incrementally indexes its own append-only completion ledger and rechecks the
current list after a pass, without restarting the service. In this mode,
lists are capped at 10,000 lines / 1,024 characters per line. Completion
membership uses keyed lookups, not a rescan of all completed pages each pass.
The existing one-shot invocation and recipe are unchanged by default.
Missing helpers, input replacement/truncation, invalid metadata and partial
completion records fail explicitly rather than pretending the cache is ready.

## Offline evidence

The JPEG tests cover new damage batches, new manifests, already converted
pages, unequal shard progress through the reused ordering tests, overlapping
refresh refusal, partial records, bounded cursor catch-up, full lists,
restart, recipe mismatch, and interrupted atomic publication. They use
temporary directories only.

A separate CLI check used an actual four-row Parquet manifest, five indexed
fixture scans and two real fixture completion files: the first tick assigned
two pages; after appending those completions, the second assigned the other
two while reading **zero** old index lines. The four published identities
were distinct; the unlisted fifth page in that same batch was excluded.

A synthetic one-million-pending-page SQLite check on the local macOS host
produced the following observations, not production throughput guarantees:

| Observation                                              |           Result |
| -------------------------------------------------------- | ---------------: |
| Assigned pages, two bounded lists                        |            4,000 |
| Allocation plus list publication                         |         0.0521 s |
| Traced Python peak during that operation                 |    465,905 bytes |
| Full-list/backpressure tick                              |         0.0002 s |
| Whole fixture process maximum RSS, including DuckDB work | 66,076,672 bytes |

`EXPLAIN QUERY PLAN` is regression-checked for a covering
`pending_order(owner,batch,eligible,doc,path)` lookup without a temporary sort.
This is a one-million-row synthetic measurement plus indexed-query evidence,
**not** an executed 20M-page live acceptance.

## Deployment and acceptance still required

First reconcile the two legacy consumers with the active follower under the
quality owner's coordination; do not install a competing producer. If legacy
JPEG conversion remains required, the owner can qualify this path as follows:

1. Retain the exact currently deployed recipe/Pillow pin. Install the cache,
   refresh and list scripts together in an isolated candidate prefix. Do not
   treat this work as authorization to upgrade a deployed recipe.
2. Provide the actual frozen wave manifest, bounded damage-index names,
   current shard batch positions, and both **complete, recipe-matching**
   completion ledgers. A remote ledger replica must remain append-only:
   append verified new bytes, not replace the file with a fresh download.
   The refresher rejects inode changes, truncation or changes around its
   checkpoint. It assumes the producer's committed prefix is append-only;
   the small anchor is not a full-prefix cryptographic audit.
3. Use a new state directory outside inputs and the scan archive. Start with
   finite low/high watermarks chosen from measured conversion rates, then
   invoke the refresher periodically from an owner-controlled timer:

   ```sh
   python3 quality-jpeg-refresh.py \
     --index /path/to/local/index/full.jsonl --scans-root /path/to/scans \
     --manifest /path/to/frozen-wave.parquet --damage-index /path/to/damage.txt \
     --done /path/to/local/done-RECIPE.txt \
     --done /path/to/remote-replica/done-RECIPE.txt \
     --recipe RECIPE --state /path/to/new-refresh-state \
     --current '0=BATCH_ver01,1=OTHER_ver02'
   ```

4. Bind host 0 and host 1 permanently to the two generated `host-N.txt`
   lists. Give each converter a readable local atomic copy of its own list
   and enable the opt-in keyed/follow flags, preserving its existing low CPU,
   I/O, memory, identity and scan-server permissions. The follow loop polls
   every 300 seconds by default. Keep remote ledger replication and list
   delivery on the same cadence; never shuffle ownership or replace a
   running process's in-flight input.
5. Demonstrate both hosts consume a bounded wave, replenish after exhaustion
   without an operator rebuilding a list, resume after restart, preserve
   completed identities and refuse conflicting recipe metadata. Measure
   actual refresh/replication memory and work, conversion rates and runner
   cache hits. Verify failed conversions remain retryable and original
   scans and the active follower are untouched. Only then enable the timer
   durably or retire the legacy path with equivalent evidence.

Rollback is to stop the candidate refresh timer and follow-mode converters,
restore their prior invocation/list paths, and retain all ledgers, generated
lists and SQLite state for reconciliation. No source fetch, cloud grant,
worker pause, admission change or production installation was performed.
