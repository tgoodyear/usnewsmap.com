# Local BOINC download host

BOINC's scheduler, application, database and result handling ran in Azure when
this page was written; they moved to an on-prem host on 21 October 2026
([boinc-onprem.md](boinc-onprem.md)). Bulk downloads and suitable background
processing should run locally to reduce spend above the subscription's verified
$150 monthly credit. Issue #783 tracks the migration; production changes are
coordinated in #529.

## Read-only download origin

`boinc/scripts/file-host.conf` is an nginx site for a dedicated, isolated file
host. Install it under `/etc/nginx/sites-enabled/` on a dedicated Debian container,
remove that container's default nginx site, and run `nginx -t` before reloading.
The container must have a **read-only** bind mount at
`/srv/usnm-boinc-downloads`. Publish from a separate trusted writer, not from nginx.
Do not mount the scan cache, project share, result quarantine or credentials
into the serving container.

The origin listens on port 8080. It is **not a public HTTPS endpoint**: use a
separate, explicitly configured TLS ingress before assigning volunteer downloads.
Do not expose the existing LAN scan server; that service also supports deletes
and manifest writes.

Only `GET` and `HEAD` for a lowercase 64-character SHA-256 filename are served.
There are no listings or upload routes, and symlinks are rejected. nginx supports
single byte ranges for interrupted downloads. At most four concurrent transfers
per source address and sixteen overall are allowed, each limited to 2 MiB/s.
These are concurrency/transfer limits, not a durable daily bandwidth quota.
The writer must verify file bytes against the filename before atomic publication.
Never put private results or database dumps in this directory.

Before using a download URL in a real workunit:

1. Verify the selected bytes and existing BOINC application signatures; a hash
   filename does not replace BOINC signing or engine identity checks.
2. Fetch the exact URL from outside the LAN over HTTPS, verify its size and
   SHA-256, and check a ranged request. TLS termination must preserve the path.
3. Confirm that the relevant submission path uses that URL rather than copying
   the same bytes to Azure. An origin by itself does not reduce cloud writes.
4. Observe a real volunteer transfer and successful result return before removing
   redundant cloud copies. Retain a rollback path for already issued work.

No scheduler URLs, account identity, allocation, trust, validation or publication
gates change. Application bundles and model files need their own signed-payload
and sticky-file integration; do not assume a native OCR scan pilot covers them.
Local result upload serving is also separate and must preserve native upload
authentication, quotas, quarantine and collection.

## Archive cost containment

On 17 October 2026, automatic scan-archive top-up and verification timers on the
local uploader were disabled under the local-first cost decision. No running
transfer was interrupted, no local or Azure scan was deleted, and manual service
units and upload checkpoints were retained. New local scans therefore **must not
be assumed to have off-site copies**. Earlier verification remains historical
evidence, not current archive coverage.

To explicitly resume the old schedule after reviewing the cost and backup policy:

```sh
sudo systemctl enable --now usnm-scan-archive.timer usnm-scan-archive-verify.timer
```

This pause avoids future scheduled archive operations, not BOINC project-share
writes, existing storage-capacity charges or early-deletion charges. Do not delete
Cold blobs solely to reduce this month's bill without calculating minimum-tier
retention costs and verifying a replacement copy.
