# Congress pages and the Serial Set

Part of the [operations guide](README.md).

## Congress pages

The Congressional Globe (#229 phase 1, [04 §4.10](../design/04-data-sources-and-ingestion.md#410-congress-pages-229)) is fetched from UNT's page OCR by `usnm-ingest congress-globe`, and the Serial Set (phase 2) from GovInfo by `usnm-ingest congress-serialset` ([below](#the-serial-set-from-govinfo)). A release with `USNM_CONGRESS` indexes both in one Congress set. All are off by default, and nothing has run yet.

1. **The archive first.** UNT's responses go to the raw store, so a fetch that stops resumes without asking UNT again. That is about 75 GB of OCR JSON before compression. Set `USNM_ARCHIVE_ACCOUNT` ([Archival storage](environments-and-deploys.md#archival-storage)) before the first fetch (decided on #229, 10 October); without a raw store the job logs a warning, keeps nothing and refetches a stopped item from its first page.
2. **The job.** Deploy `usnm-cong-caj-<env>` (saved as `CONGRESS_JOB`): manual, one replica of 0.5 vCPU and 1 GiB, on the ingest image as `id-usnm-ingest`.

   ```sh
   scripts/settings.sh --env prod --key USNM_CONGRESS_JOB --value true
   scripts/provision.sh --env prod
   RG=$(scripts/settings.sh --env prod --key AZURE_RESOURCE_GROUP)
   az containerapp job start -n "$(scripts/settings.sh --env prod --key CONGRESS_JOB)" -g "$RG"
   ```

   Each execution lists the collection, leaves out the items another holds, and fetches the items without a finished attempt. It goes one request every 3 seconds, as UNT's robots.txt asks, and stops between pages at 22 hours. Start it again after each execution until its report (JSON, at the end of the log) has `"left": 0`. The whole Globe is about 105,000 pages, about 4 days. The log has `Congressional Globe items` with the counts, `Globe item curated` per item with its pages by `date_from`, and `the server asked us to wait` (with `server=UNT`) on a 429 or 5xx. One item, or a fetch again: `scripts/start-job.sh --env prod --job CONGRESS_JOB -- congress-globe --items metadc29274 --max-runtime-secs 79200 [--refetch]` (the arguments after `--` replace the job's, so keep its `--max-runtime-secs`). The Cosmos lock `congress-globe` refuses a second execution while one runs.

3. **The release.** Turn on `USNM_CONGRESS` (the ingest job's `--congress`), then provision. The next ingest run builds `congress-<stamp>-<n>` from the finished items, even with no new LoC batch (`no newly curated batches; releasing the changed Congress pages on the same indexes`). Later runs keep it until the fetch writes more items. Check `reference/<version>/congress.json` (the items and pages indexed), `congress/baselines.json` and `current.json`'s `congress` block. The API serves the pages with `source=congress` while `USNM_CONGRESS_SEARCH` is on (the default; [configuration](../configuration.md)).

Clearing `USNM_CONGRESS` makes the next release publish a version without the Congress set, even with no new LoC batch (`no newly curated batches; releasing the same indexes without the published Congress set`); `USNM_CONGRESS_SEARCH=false` hides it without a release. A full rebuild with `USNM_CONGRESS` on rebuilds the Congress index too when the analyzer changes.

### The Serial Set from GovInfo

`usnm-ingest congress-serialset` (#229 phase 2, [04 §4.10](../design/04-data-sources-and-ingestion.md#410-congress-pages-229)) needs no API key: GovInfo's sitemaps, MODS and PDFs are public URLs. About 222,000 documents in about 11,900 volumes: 444,000 requests and about 4 TB of PDFs, each streamed to the replica's disk, read with `pdftotext` and deleted (about 5 days at a request a second).

1. **The archive first**, as for the Globe: the MODS and each document's page texts (`pages.json`, about 20 GB in all) go to the raw store under `congress/govinfo/{volume}/{package}/{lastmod}/`, so a stopped volume resumes without downloading its PDFs again. The PDFs are never kept. Without a raw store the job logs a warning and refetches a stopped volume's PDFs.
2. **The job.** Deploy `usnm-sset-caj-<env>` (saved as `SERIALSET_JOB`): manual, one replica of 1 vCPU and 2 GiB, on the ingest image (which carries poppler-utils) as `id-usnm-ingest`.

   ```sh
   scripts/settings.sh --env prod --key USNM_SERIALSET_JOB --value true
   scripts/provision.sh --env prod
   RG=$(scripts/settings.sh --env prod --key AZURE_RESOURCE_GROUP)
   az containerapp job start -n "$(scripts/settings.sh --env prod --key SERIALSET_JOB)" -g "$RG"
   ```

   Each execution reads the sitemap index and the 182 yearly sitemaps (all of them even with `--years`), then fetches the volumes without a finished or empty attempt, and those whose packages differ from the ones their newest attempt read (`source_packages`, a fingerprint of each package id and its `lastmod`: a document GPO changed, added or dropped, whatever its `lastmod`). An attempt written before the fingerprint was kept has none, so its volume is fetched once more (from the raw store's copies) and counted in `changed`. It goes about one request a second, backs off on 429 and 5xx (`the server asked us to wait`, `server=GovInfo`), and stops between documents at 22 hours. A sitemap or MODS request may take 120 seconds a try and a PDF 30 minutes, and every request ends by an hour after the deadline (a try that couldn't start with time left, and a retry past then, fail the request), so the job finishes and unlocks before its 24-hour timeout. Start it again after each execution until its report (JSON, at the end of the log) has `"left": 0` and `incomplete` empty. The log has `Serial Set volumes` with the counts, `Serial Set volume curated` per volume, and a warning per document not read. A volume is written only when every document is read or left out for good; otherwise it is in `incomplete` (with how many documents weren't read), nothing is written for it and the next execution fetches it again (what was read is in the raw store). The report lists the documents: `missing` (GovInfo answered 404 for the MODS, or for a PDF the MODS names; never kept, so asked for again) and `unreadable` (pdftotext failed, or MODS that isn't MODS), which keep their volume `incomplete`; `no_pdf` (the MODS names no PDF and GovInfo has none) and `undated` (no usable `dateIssued` or session year), left out for good; and `empty_volumes` (every document left out for good: an empty attempt, `empty.json` with the documents and why, is written in place of pages, so later executions skip the volume until GPO changes its packages or `--refetch`). A volume still `incomplete` after a few executions with the same document is a file GovInfo lacks or pdftotext can't read: look at the document on GovInfo before deciding what to do. `date_precision` counts documents dated to the day, month or year. Some volumes or years: `scripts/start-job.sh --env prod --job SERIALSET_JOB -- congress-serialset --years 1820,1821 --max-runtime-secs 79200 [--serials 33] [--refetch]` (the arguments after `--` replace the job's, so keep its `--max-runtime-secs`: without it the job has no deadline and a long run can hit the 24-hour timeout before it unlocks). `--years` picks the volumes with a package in those years' sitemaps; each is still made of its packages in every sitemap, so it is written whole and a later run of all years skips it. The Cosmos lock `congress-serialset` refuses a second execution while one runs; it is separate from the Globe's, so the two fetches can run together.

3. **Updates.** GPO is still adding volumes. Run the job again monthly: it rereads the sitemaps and fetches only new or changed volumes. The next release with `USNM_CONGRESS` rebuilds the Congress set.
4. **Check.** After the release, `current.json`'s `congress.series` has `serialset` with its pages and dates (1817 to 1998), and `source=congress&series=serialset` searches only its pages. A hit links to the page in the document's PDF (`…/content/pkg/{id}/pdf/{id}.pdf#page={seq}`).

### The Serial Set on-prem

The same fetch can run on an OCR node instead of as `usnm-sset-caj-<env>`, to spend no Azure compute ([ADR-0008, amendment of October 2026](../design/adr/0008-private-networking.md#amendment-october-2026-the-on-prem-serial-set-runner-229), [08 §8.3.2](../design/08-azure-infrastructure.md#832-on-prem-serial-set-runner)). Don't start the Azure job while the node runs it: the node's lock is in a local file, which the job's Cosmos lock doesn't see.

1. **Settings.** `USNM_SERIALSET_ONPREM=true` in `infra/env/<env>.json` (through a pull request), and the node's Arc identity's object id in the private setting `USNM_SERIALSET_RUNNER`. The lake's firewall takes its addresses from `USNM_OCR_NODE_IPS`, which the exchange account already uses; check it still holds the home IP (`curl -s https://api.ipify.org` on a node).

   ```sh
   az resource show -g rg-usnm-prod -n usnm-ocr-node3-arcs-prod --resource-type Microsoft.HybridCompute/machines --query identity.principalId -o tsv
   scripts/settings.sh --env prod --key USNM_SERIALSET_RUNNER --value <object id>
   scripts/provision.sh --env prod --what-if   # the policy step adds the lake; the stack adds the IP rule and the grant
   scripts/provision.sh --env prod
   ```

   A role assignment can take a few minutes to apply.

2. **The node.** Build `usnm-ingest` from source in a container, so the node gets no toolchain, and install it with poppler-utils and the units in `scripts/ocr-node/` (`usnm-sset.service`, `usnm-sset-status.service`, `usnm-sset-status.timer`, `sset-status.py` as `/usr/local/bin/usnm-sset-status`):

   ```sh
   mkdir -p src
   curl -sL https://codeload.github.com/tgoodyear/usnewsmap.com/tar.gz/<commit> | tar xz -C src --strip-components=1
   nice -n 19 podman run --rm --cpus 3 -v "$PWD/src:/src:Z" -w /src docker.io/library/rust:1-bookworm \
     cargo build --release --locked -p usnm-ingest --bin usnm-ingest
   sudo dnf install -y poppler-utils
   sudo install -m 0755 src/target/release/usnm-ingest /usr/local/bin/
   sudo install -m 0755 src/scripts/ocr-node/sset-status.py /usr/local/bin/usnm-sset-status
   sudo install -m 0644 src/scripts/ocr-node/usnm-sset.service src/scripts/ocr-node/usnm-sset-status.service \
     src/scripts/ocr-node/usnm-sset-status.timer /etc/systemd/system/
   sudo install -d -o usnm -g usnm -m 0750 /var/lib/usnm-sset /var/lib/usnm-sset/raw /var/lib/usnm-sset/reference
   sudo install -d -m 0755 /etc/usnm
   # Create only if absent; preserve an existing local endpoint on upgrades.
   sudo test -e /etc/usnm/sset-status.env || sudo install -m 0600 /dev/null /etc/usnm/sset-status.env
   sudoedit /etc/usnm/sset-status.env
   # Before continuing, set USNM_SSET_STATUS_PUSH_URL to the local dashboard's
   # full http://<local-dashboard>/api/sset/usnm-sset endpoint (not a placeholder).
   sudo systemctl daemon-reload
   sudo systemctl enable --now usnm-sset-status.timer
   sudo systemctl enable usnm-sset
   ```

   `/etc/usnm/sset.env` (mode 0640, group `usnm`) sets `USNM_CURATED_URL=https://<lake>.blob.core.windows.net/curated`, `USNM_RAW_URL=/var/lib/usnm-sset/raw`, `USNM_STATE_FILE=/var/lib/usnm-sset/state.json` and `USNM_REFERENCE_URL=/var/lib/usnm-sset/reference` (an empty directory; the fetch reads nothing there). The raw store holds the MODS and page texts, about 20 GB, so a stopped volume resumes without its PDFs. Before the lake is open, `USNM_CURATED_URL` can be a local directory: the fetch then fills the raw store, and a later run against the lake reads every volume back from it without asking GovInfo for the PDFs again.

3. **Run.** `sudo systemctl start usnm-sset` runs the whole fetch, at a request a second, until every volume is written or left (no deadline), and then stops. It runs as `usnm` with the Arc identity. Logs: `journalctl -u usnm-sset`; the report is the JSON at the end. `/var/lib/usnm-sset/status.json` (every 10 minutes) has the volumes to fetch, written, incomplete, the rate per hour and the hours left, and `finished` once `/var/lib/usnm-sset/done` exists. The same timer pushes it to the OCR dashboard (`PUT /api/sset/usnm-sset` over plain HTTP, accepted from this node's address only; the dashboard needs `--sset-push usnm-sset=<this node's name>` in the dashboard's infrastructure configuration), which shows it under Serial Set.

   The unit is enabled, so it also starts at boot, until the fetch has finished. A run that ends with `"left": 0` and `incomplete` empty writes `/var/lib/usnm-sset/done` (`--done-file`: when, and the volumes and pages), and the unit doesn't start while that file exists (`ConditionPathExists=!`). A run that ends with incomplete volumes (a 404 for a document the sitemap lists, a PDF that couldn't be read) exits 0 without the marker, and stays down until the next boot or `sudo systemctl start usnm-sset`, which fetches those volumes again. If the same volumes stay incomplete run after run (GovInfo answers 404 for good), check them on GovInfo, then write the marker by hand: `sudo -u usnm touch /var/lib/usnm-sset/done`. To fetch again after it's done (new volumes on GovInfo), remove the marker and start it: `sudo rm /var/lib/usnm-sset/done && sudo systemctl start usnm-sset`.

   Restarts don't wait for the fetch's lock (`congress-serialset`, in `USNM_STATE_FILE`, a 30-minute lease renewed every 5 minutes). With a local state file the holder id is `{host}:{boot id}:{pid}:{nanos}`, and a new run takes the lock over at once when the holder was on the same host and is gone: from an earlier boot, or from this boot with its process no longer running (it logs `took over the lock of a run that is gone`). A holder on another host, a live process or an id in another form still waits out the lease. A failure releases the lock, and so does SIGTERM (`systemctl stop`): the fetch drops what it was doing, releases the lock and exits 143, which the unit counts as a clean stop. A volume stopped mid-way writes nothing, and the next run resumes it from the raw store. The Azure job's Cosmos lock is held as before. A failure starts it again after 60 s, then about 2, 4 and 8 minutes, then every 15 minutes (`RestartSteps`), so a GovInfo outage longer than the fetch's own retries doesn't hammer it; the count starts again at boot or with `sudo systemctl reset-failed usnm-sset`. To stop it for good before it's done: `sudo systemctl disable --now usnm-sset`.

4. **Close.** When it's done, set `USNM_SERIALSET_ONPREM` to false and provision: the lake closes, the grant goes and the policy drops the exemption. Remove the private setting with `scripts/settings.sh --env prod --key USNM_SERIALSET_RUNNER --unset`.
