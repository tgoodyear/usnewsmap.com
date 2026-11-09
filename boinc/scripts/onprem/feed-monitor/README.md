# Feed monitor deployment (#1441)

The feed sources (`usnm-boinc-feed-source@.service`) and the evaluator (`usnm-boinc-feed-health.service`) run `feed_collect.py` and `feed_status.py` from `/opt/usnm-feed-monitor/`. Those two import `client_account.py` and `acceptance_client.py`, so all four ship together. Hosts follow what CI publishes from `main` by pulling it, as on-prem serving follows the API image ([scripts/onprem/serving](../../../../scripts/onprem/serving/README.md)). Nothing pushes into the hosts.

## How a change reaches the hosts

1. A merge to `main` that changes one of the four scripts runs [feed-monitor-release.yml](../../../../.github/workflows/feed-monitor-release.yml). It runs the feed monitor tests, builds [bundle.sh](bundle.sh)'s archive, attests it and `SHA256SUMS` (build provenance), and publishes them twice:
   - as the release `feed-monitor-<yyyymmdd>-<sha7>`, which never changes;
   - as the assets of the release `feed-monitor-latest`, which are replaced each time and only move forward along `main`.
2. On each host, `usnm-feed-monitor-update.timer` runs [update.sh](update.sh) 2 minutes after the last run ended. It reads `feed-monitor-latest`'s `SHA256SUMS` from the release download (not the API, so it costs no rate limit) and stops if that names the installed archive.
3. For a new archive it downloads it, checks the sum, reads its attestation from the public API with no token and verifies it with `gh attestation verify --bundle` (signer `feed-monitor-release.yml`, ref `refs/heads/main`). It then checks the contents: exactly the bundle's files, as regular files, matching `VERSION.json`, whose commit must be the attested one and the installed one or a descendant of it (the public compare API says which). It also checks that both scripts start.
4. It unpacks the archive into `/opt/usnm-feed-monitor-releases/<version>/` and points the symlink `/opt/usnm-feed-monitor` at it in one rename. First it stops the feed timers, so no run starts, and waits up to 120 s for any running feed unit to finish; then it starts the timers again. Nothing else restarts: the next timer run uses the new files.

An archive that fails a check is rejected (`/var/lib/usnm-feed-monitor-update/rejected`) and never tried again. A sum mismatch on `feed-monitor-latest` is a publication in progress and is retried, and so are API errors and a gh that can't load its trusted signing data. Deploys and rejections are logged at Warning:

```sh
journalctl -u usnm-feed-monitor-update -p warning
readlink /opt/usnm-feed-monitor
jq . /opt/usnm-feed-monitor/VERSION.json
```

The updater, its units and the feed units themselves aren't in the bundle. A change to them still needs `install.sh` (or the unit install in [boundaries-and-monitoring.md](../../../docs/boundaries-and-monitoring.md#installing)) on each host.

## Install

On each feed host (today the controller, evaluator, server, backlog and own-client sources), as root, from a checkout of reviewed `main`:

```sh
boinc/scripts/onprem/feed-monitor/install.sh --dry-run
boinc/scripts/onprem/feed-monitor/install.sh
```

It installs `jq` if missing, the GitHub CLI release pinned in `install.sh` (checked against its sha256) as `/usr/local/libexec/usnm-feed-monitor/gh`, the updater as `/usr/local/sbin/usnm-feed-monitor-update`, and its service and timer. Then it runs the updater once and enables the timer. The first run moves the hand-copied files in `/opt/usnm-feed-monitor/` aside to `/opt/usnm-feed-monitor.pre-ci-<time>/`, untouched, then links the release. It doesn't change the feed units, their environment files or `feed-monitor.json`.

Check that every host reports the same version:

```sh
jq -r .version /opt/usnm-feed-monitor/VERSION.json
```

## Rollback and pinning

```sh
usnm-feed-monitor-update --rollback                                 # back to the previous release, then hold
usnm-feed-monitor-update --version feed-monitor-20261108-0123abc    # that release, older ones too, then hold
usnm-feed-monitor-update --dry-run                                  # download and verify, change nothing
rm /etc/usnm-feed-monitor-update/hold                               # follow main again
```

While `/etc/usnm-feed-monitor-update/hold` exists, timer runs do nothing. The three newest releases stay on disk, and always the live and the previous one. A host's first release has no previous one: to go back to the hand-copied files, `rm /opt/usnm-feed-monitor`, move the `.pre-ci-*` directory back, and create the hold file.
