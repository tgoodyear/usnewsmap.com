# The quality wave's page JPEG cache

**Status:** retired; the on-prem quality feed derives page JPEGs itself · **Date:** October 2026 · **Measured on:** CT 211's converter from 19 October 2026 (pages a second at 2 and 6 cores) · **Code:** `scripts/ocr-node/quality-jpeg-cache.py`, `quality-jpeg-list.py`, `usnm-quality-jpeg-cache.service`, `usnm-quality-jpeg-cache-remote.service` · **Issues:** #850, #915

A cache of page JPEGs for the Azure-hosted quality wave, converted on the scan-fetch container and other LAN hosts instead of in the wave's Container Apps job. The [on-prem quality feed](../boinc/apps/quality.md#on-prem-feeder-v1-wave10-on) replaced it. Moved here from the BOINC scan-cache hand-off page on 10 November 2026; [bounded JPEG-cache replenishment](2026-10-28-jpeg-cache-replenishment.md) is the later design work.

## The cache and its retirement

**Legacy Azure-wave optimization, not the current on-prem quality feed.**
The `v1-wave10` follower in the batch container derives page JPEGs directly
from the scan server with `quality_feed.py page_jpeg`; it does not read either
legacy converter's fixed list or the `quality-jpeg/` blob cache. Its fetched-time,
then doc-id order, cross-campaign database deduplication, durable completion
records and READY-chunk backpressure replace that list's replenishment role.
Do not install another list-refresh producer alongside this follower.

Retire the exhausted legacy services without deleting their scripts, lists,
completion ledgers or cached blobs. The local converter in the scan-fetch
container is already disabled. For the remote converter on the laptop **host**,
first verify it is inactive, retain its unit and enablement receipt, then:

```sh
if test "$(systemctl show usnm-quality-jpeg-cache-remote.service -p ActiveState --value)" = inactive; then
  systemctl disable usnm-quality-jpeg-cache-remote.service
else
  printf '%s\n' 'Refusing retirement: legacy converter is not confirmed inactive.' >&2
  false
fi
```

Verify the batch container's `usnm-quality-follow.service` remains running and
the host's quality ship/results timers remain enabled and active. Disabling an
already-inactive converter must not start, stop or restart any of those units.
Rollback restores only its prior boot enablement with
`systemctl enable usnm-quality-jpeg-cache-remote.service`; it does not start
the exhausted list or establish recipe provenance. In particular, never
relabel the retained `done.txt` as `done-l-q75-r2.txt`.

The following recipe and installation procedure remain available for an
explicitly selected legacy runner; they are not the current feed's prerequisites.

A quality wave's runner turns each archived page into the worker's `page.jpg`: grayscale,
JPEG quality 75, and a JPEG2000 master over 4000 px decoded at half resolution. Decoding
JPEG2000 is most of its CPU; the former Container Apps job had 4 vCPUs (#850).
The scan-fetch
container holds the masters, so `scripts/ocr-node/quality-jpeg-cache.py` does the same
conversion there and puts each page in the hand-off container as
`quality-jpeg/l-q75-r2/<blob name>.jpg`. Blob metadata carries the master's sha256, the
recipe, the Pillow version, the size and the JPEG's sha256. The runner takes the cached page
when it is there and converts the master otherwise. A grayscale JPEG master under 40 Mpx and
20 MB goes to workers as it is, so the cache skips it.

`scripts/ocr-node/quality-jpeg-list.py` builds the page list: JPEG2000 pages only, in the
runner's shard order. The runner gives shard i every eighth batch, the manifest's first
(largest first), then tier 1: damage-index batches, in name order, that are not in the
manifest and have archived scans. It walks each batch's pages in doc_id order. The list
interleaves the shards, so the cache keeps ahead of all of them. Aim it past each shard's
current batch with `--current s=<batch>_verNN`, from the shard's status file
(`quality/v1-wave9/status/shard-<s>.json`, `current_batch`): the runner skips pages that are
already bundled or have no LoC text, so a shard's bundle count doesn't tell where it is in
its list. The runner lists the archive and the damage index once, when it starts, and both
grow, so a later list can't reproduce its tier-1 rotation. Each shard's list therefore
continues with every tier-1 batch after the one it names, and the lists are deduplicated: the
cache converts some pages another shard has passed, but none a shard still needs. Rebuild the list before the converter reaches
its end, or the cache falls behind as shards move into tier 1.

The output must be byte for byte what the runner would make, so the script runs only on
Pillow 13.0.0 from the same pinned wheel as the bridge image
(`boinc/images/bridge/Containerfile`). It checks each master against its fetcher
sidecar and never replaces a blob. An existing blob must match the size and every
provenance field (source and JPEG sha256, recipe, Pillow version, width and height);
missing or different metadata stops the run without marking that page done. Reconcile
the existing blob with the wave's maintainer rather than overwriting it or retrying with
weaker checks. The script lists done
pages in `/var/lib/usnm-quality-jpeg-cache/done-l-q75-r2.txt`. The exchange account's lifecycle policy deletes cached
pages 30 days after they are written (`expire-boinc-handoff-quality-jpeg`, #915), so a wave
must bundle a page within 30 days of its conversion or the runner converts the master again. `usnm-quality-jpeg-cache.service`
runs it under a dynamic user in the `himds` group, for the Arc token. It runs at low CPU
and I/O priority, so the prefetcher and the scan server go first. CT 211 has 6 cores and
8 GiB for it (`WORKERS=6`), from 19 October 2026: with 2 cores it converted about 1.1
pages a second, slower than the runner used pages, and with 6 about 2.9. This keeps the
work on pve-big rather than adding vCPUs to the runner's job.

The Pillow 13 upgrade uses recipe `l-q75-r2` and a recipe-specific completion
ledger so it neither reads Pillow 11's `l-q75-r1` blobs nor skips pages listed
in the old `done.txt`. Do not copy the old ledger into the new one. Stop the
converters before upgrading their wheel and code, rebuild their page lists
without the old completion ledger, and deploy the matching bridge and runner
before resuming. Check for a `JPEG_CACHE_PREFIX` override: the new runner must
use `quality-jpeg/l-q75-r2/`. Existing submitted work and r1 blobs stay unchanged;
retain the old image and scripts for rollback. When replacing an extracted wheel,
extract into a fresh directory, not over the old Pillow installation.

```sh
# On the operator's machine: the page list from the wave's manifest. --start-after s=k
# starts shard s after its batch k (0-based; the batch its status file names now), so the
# cache stays ahead of the runner. Copy the list, the script and the unit into CT 211's
# /root (pct push).
# damage.txt: az storage blob list on the lake's curated container, --prefix audit/damage/v2/.
# keep-files.txt: in CT 211, cd /tank/ocr/scans && find keep -type f (all formats: JPEG-only
# batches still take their turn in the rotation; only JPEG2000 pages are listed).
scripts/ocr-node/quality-jpeg-list.py --manifest wave9_manifest.parquet --out pages.txt \
  --damage-index damage.txt --keep-files keep-files.txt \
  --current 0=dlc_friendship_ver03,1=dlc_eject_ver01,...
# For later lists, add --done done-l-q75-r2.txt once that r2 ledger exists.
# The first r2 list must not use an r1 ledger or a nonexistent r2 ledger.
# In CT 211, as root: the pinned Pillow wheel, as the bridge image installs it.
d=/usr/local/lib/usnm-quality-jpeg-cache
mkdir -p "$d/py" && cd "$d"
curl -fsSLo pillow.whl https://files.pythonhosted.org/packages/36/46/f509b74787f40ce0627501089d630b2c741b63b244c1f40b640b3b01e181/pillow-13.0.0-cp311-cp311-manylinux_2_27_x86_64.manylinux_2_28_x86_64.whl
echo "a625df9a2a5597fa5e5266e01aefc82604bb85d291e3547981e863560d75da6c  pillow.whl" | sha256sum -c
python3 -m zipfile -e pillow.whl py && rm pillow.whl
install -m 0755 /root/quality-jpeg-cache.py "$d/"
install -m 0644 /root/usnm-quality-jpeg-cache.service /etc/systemd/system/
printf 'ACCOUNT=<exchange account>\nLIST=%s/pages.txt\nWORKERS=6\n' "$d" > /etc/usnm/quality-jpeg-cache.env
install -m 0644 /root/pages.txt "$d/"
systemctl daemon-reload && systemctl start usnm-quality-jpeg-cache
journalctl -fu usnm-quality-jpeg-cache
```

Another LAN host with idle CPU can convert too, reading masters from the scan server
(`--scan-server`, `usnm-quality-jpeg-cache-remote.service`) rather than `/tank`. It needs
three things. The first is its Arc machine identity in the on-prem hosts' group,
`usnm-onprem-grp-<env>` (`USNM_ONPREM_GROUP`, `scripts/ocr-node/group.sh --kind onprem`),
which has the hand-off writer role (list, read, create; no delete). Not the OCR nodes'
group: its member count sets the nodes' LoC pacing. The second is the host on the scan server's `--allow`
(`USNM_SCANSERVE_ALLOW` in CT 211's `/etc/usnm/scanserve.env`, then restart
`usnm-scanserve`). The third is its own share of the list: split the pages that remain
alternately, so the two hosts never convert the same page and both stay just ahead of
the runner. Install the Pillow 13.0.0 wheel for the host's Python, as above (cp313:
`pillow-13.0.0-cp313-cp313-manylinux_2_27_x86_64.manylinux_2_28_x86_64.whl`, sha256
`5f56499c1a74d2b862beb0c33b8016f99d095858455df9d21583bbbc1c15b582`). Before starting it,
check that a few of its JPEGs match CT 211's byte for byte (`--what-if` prints the size;
compare the sha256 of pages both convert).

```sh
# Stop CT 211's converter first: it loads its whole list at start, so a split taken while
# it runs would leave pages queued on both hosts.
pct exec 211 -- systemctl stop usnm-quality-jpeg-cache
# On the operator's machine: what CT 211 hasn't done, split alternately.
grep -vxFf <(cut -f1 done-l-q75-r2.txt) pages.txt > left.txt
awk 'NR % 2 == 1' left.txt > pages-ct211.txt
awk 'NR % 2 == 0' left.txt > pages-remote.txt
az ad group member add --group <USNM_ONPREM_GROUP> --member-id <host's Arc principal id>
# Install each half as its host's LIST (/usr/local/lib/usnm-quality-jpeg-cache/pages.txt),
# then start both. A host added to the group needs a fresh Arc token: restart himdsd.
# On the other host, as root, after the Pillow wheel and the script are in place:
install -m 0644 usnm-quality-jpeg-cache-remote.service /etc/systemd/system/
printf 'ACCOUNT=<exchange account>\nLIST=%s/pages.txt\nWORKERS=6\nSCAN_SERVER=http://<scan-fetch host>:8380\n' \
  /usr/local/lib/usnm-quality-jpeg-cache > /etc/usnm/quality-jpeg-cache.env
systemctl daemon-reload && systemctl start usnm-quality-jpeg-cache-remote
# Later lists: append each host's half to its pages.txt and restart it; done-l-q75-r2.txt skips
# what it has done.
```
