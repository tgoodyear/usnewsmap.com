# Retained original-source selection

The Qwen feeder selects damaged non-English pages only from indexed scans whose
exact `(batch, batch_version)` archive file is currently retained locally.
Before each selection it asks the existing restricted archive helper for a fresh
metadata-only inventory (empty JSONL input), then filters the scan index before
the normal damage/language/image-availability selection. No archive decompression
or new network source fetch is needed for this inventory.

This fixes a measured source-supply bottleneck: on 29 October, only 1,425 of
10,000 scored catalog candidates referenced a retained archive, including only
21 of the highest-ranked 100. Repeatedly choosing missing archives wasted
selection capacity and could obscure useful locally available supply.
These are candidate counts, not source-proven jobs or ready-queue depth.

An archive file's existence **does not establish** that the desired ALTO member
exists, is intact or yields useful crops. Existing exact member extraction,
hash-bound provenance, real source pixels, crop dispositions, deduplication,
native audited publication, storage limits and independent-host quorum remain
unchanged. All indexed/catalog pages remain upstream; filtering does not delete
them, permanently defer missing archives or fabricate scientific work. Newly
retained archives are considered on the next tick.

Deploy the additive `archive_alto.py` report extension first, preserving the
existing forced SSH command. Its legacy `cached`/`missing`/`pending` fields are
unchanged. Then deploy `qwen_feed.py` at a producer idle boundary. The new
producer fails explicitly if the archive helper lacks the inventory field;
it never silently treats missing evidence as available sources.
No shipper, worker recipe, GPU admission, bundle or validator update is required.
`retained_archive_filter` timing/count receipts distinguish candidates filtered
from actual prepared pages and newly audited workunits. Temporal schedules the
same native producer service; do not start a competing producer for this change.
