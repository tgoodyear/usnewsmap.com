# ADR-0022: Cold data moves to `tank` first; live index and public serving stay in Azure

- **Status:** Proposed (needs the maintainer's decisions D1 to D6 in [design 15](../15-onprem-cold-storage.md#158-decisions-for-the-maintainer))
- **Date:** 2026-10
- **Relates to:** ADR-0002 (lake as system of record), ADR-0008, ADR-0009, ADR-0015, ADR-0018, ADR-0021

## Context

The maintainer wants as much storage off Azure as is sensible, with serving on-prem as the end goal. Phase 1 is cold data only. The uplink is 1 Gbps symmetric and on-prem redundancy is a ZFS mirror (no second copy). The read-only inventory of 18 October 2026 ([design 15](../15-onprem-cold-storage.md)) found that cold data is a small part of the bill:

- Storage, private link and egress cost $149.51 in 30 days; the lake `stusnmdffapzy` is half of it, driven by the live index and its soft-deleted splits (3.6 TB), not by cold data.
- The cold data is the 1.1 TB scan archive, 67 GB of LoC archives and parts of the lake's `curated`.
- The scan archive's keep set (224,399 images, 1,056 GB) is already on `tank/ocr/scans` with matching sizes; Azure holds the only off-site copy.

## Decision

1. Phase 1 moves **no live or public-serving data**. It verifies the scans already on `tank`, copies the 73-blob archive account and a second copy of OCR ALTO to `tank`, and adds snapshots, a `usbbak` replica and monitoring.
2. Files stay files: ZFS datasets read by POSIX mounts for on-prem consumers. No S3 gateway until an Azure-hosted reader needs one (Garage, not MinIO, if so).
3. Nothing is deleted in Azure until checksums are verified, the on-prem copy has been scrubbed, and the maintainer approves the specific deletion.
4. Expansion path: larger mirror disks first, an HBA only for a third vdev, M.2 NVMe only for rebuildable data.
5. Datasets that cannot be re-derived stay in two places until a second site exists.

## Alternatives

| Option                                                      | Why not                                                                                                   |
| ----------------------------------------------------------- | --------------------------------------------------------------------------------------------------------- |
| Move the lake and live index now                            | Contradicts the Phase 1 decision; ~$100 egress for the index, an Azure reader would cross the home uplink |
| MinIO or Garage in front of `tank` from the start           | Extra service, opaque chunk storage, no second node for redundancy                                        |
| Open the archive and index accounts' firewalls to inventory | Forbidden by ADR-0008/0009; the sizes came from metrics and the existing operator rule                    |

## Consequences

- Cold-only savings are about $2/month now and about $18/month once the BOINC bridge reads scans on-prem and the Azure copy is retired.
- The larger Azure savings (soft-deleted index splits, the old `qw-index`, Premium `qw-scratch`) are Azure-side clean-ups and are separate decisions.
- The scan archive's off-site role needs an explicit decision (D1).
- `sdb` shows media errors; replacing it comes before moving more data.
