# ADR-0021: Quickwit's index has its own storage account

- **Status:** Accepted, opt-in for the #283 rebuild
- **Date:** 2026-10-15
- **Amends:** ADR-0002 (index storage), 08 §8.1
- **Amended by:** [ADR-0027](0027-onprem-serving-behind-cloudflare.md) (the separate index account, its endpoint and `publish-staged` retire after on-prem serving's Phase 4)

## Context

The lake's account-wide versioning and blob soft delete retain every split
Quickwit deletes after a merge or a prune (#245). Retention was reduced from
14 to 3 days (#416) to contain that derived-data cost, shortening the
recovery window for `curated/` and `reference/`. The maintainer decided on
12 October to separate the index at the next full rebuild (#283).

## Decision

- Gate creation and writes behind `USNM_INDEX_STORAGE=false` by default.
  The account is `usnmqwst{env}{suffix}`, named centrally under ADR-0017.
  Keep its `qw-index` container Hot and disable versioning and blob and
  container soft delete. Recovery is a retained index version or a rebuild.
- Follow ADR-0009: Entra only, no shared keys or SAS, no public access or
  trusted service bypass. Use a Blob private endpoint and the existing
  private DNS zone. Do not exempt it in `publicAccountIds`; log writes and
  deletes.
- Keep all existing lake grants. Add container-scoped Contributor for the
  writer system identity and the ingest user identity. Add Reader for the
  parallel searcher system identity and for the API system identity during
  the transition, while `USNM_SEARCHER_APP=false`. The permanent
  standalone-searcher opt-in removes the API identity and its grants (#365).
  `USNM_INDEX_STORAGE_SERVE` alone does not, so the old lake-sidecar revision
  can finish serving.
- Quickwit Azure URIs select containers, not accounts. Point the writer's
  account configuration and the parallel searcher's YAML at the new account;
  both metastore and index root stay `azure://qw-index`. Do not copy a
  metastore whose relative index URIs would resolve against the wrong blobs.
- Stage complete rebuilds of **all** index sets (unchanged Congress included)
  and their normal lake reference snapshots. Publish only
  `index-storage-pending.json`; keep `current.json` and `ops/current` intact.
  The staged run is recorded as published to that pending pointer, with the
  lake version retained as `previous_version`, and prune protects newer runs.
- Verify the parallel internal searcher, then promote the pending pointer
  and set `USNM_INDEX_STORAGE_SERVE=true`. The API's URL changes in a new
  revision; the old revision and searcher keep serving until readiness
  switches traffic. Coordinate the pointer/reader window with API image
  rolls and cold starts, as the runbook specifies. Neither flag is cleared
  afterwards.
- Keep the lake rollback through the next verified new-account release.
  Prune each account through Quickwit, dry-run first. Then restore lake
  retention with `USNM_LAKE_BLOB_SOFT_DELETE_DAYS=14` (default remains 3).
  Leave the experiment's `qw-cluster` and `qw-bench` unchanged.

## Consequences

The protected lake no longer pays to retain future merge inputs. The new
account has no blob-level recovery, and a second searcher costs compute
while cross-account rollback is retained. A rollback across accounts must
restore both the pointer and reader selection, with writers paused. A
pointer alone is insufficient. Disabling account creation is destructive
stack cleanup, never a rollback mechanism. The old empty container and
roles can be removed separately after retirement. The defaults and shared
searcher YAML stay unchanged, so merge alone moves no production data.

See [operations](../../operations.md#index-storage-cutover) for the exact
rebuild, cutover, rollback and account-specific prune procedure.

## Amendment, October 2026: staged builds from on-prem (#283, #917)

A full rebuild can be built on the on-prem release container instead of
the ingest job: the Quickwit writer works on the container's own disk
(`file://` metastore and index root) and `usnm-ingest publish-staged` then
copies the staged version in. It copies each index's published splits,
then its `metastore.json` with `index_uri` pointed at `azure://qw-index`,
then adds the index to `qw-index/manifest.json`, then the lake snapshot and
`index-storage-pending.json`. Splits and metastores are create-only, and a
rerun skips objects already there with the same size. It never writes
`current.json`: promotion and the reader switch are unchanged.

For the copy, the index account opens to the node IPs (`USNM_ONPREM_RELEASE`
with `USNM_INDEX_STORAGE`, `USNM_OCR_NODE_IPS`). Its firewall then denies
everything else, Entra ID stays the only authentication, the private
endpoint stays and the searchers keep using it. The public-network policy
exempts the account by resource id only while it is open. Turn the setting
off after the copy to close the account again. The release container's own
Arc identity holds the grants (`modules/onprem-release.bicep`); the on-prem
hosts' group does not, because only the release writes the serving index.
