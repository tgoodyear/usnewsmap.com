# Fixture corpus (synthetic)

**Everything in `data/` is synthetic.** Place and title names are labelled "Fixture", LCCNs use the unassigned `sn99…` range, and the page text is templated. It exists only for local development and tests, and it must never be presented as real Chronicling America data.

It mimics the shapes the real pipeline will produce ([04](../docs/design/04-data-sources-and-ingestion.md)):

- `current.json`: the published version, naming its sealed index set and reference snapshot
- `indexes/{index_id}.jsonl`: one engine document per page (a base index and one delta index)
- `fixture-v1/`: the reference snapshot (`places.json`, `titles.json`, `baselines.json`, `title_pages.json`, and the `manifest.json` that checksums them)

Regenerate with `python3 fixtures/generate.py` (deterministic).
