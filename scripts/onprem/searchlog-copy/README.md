# Search log copy on tank

These files copy the search log's day files (`searches/days/{day}.jsonl`) from the lake to a restricted dataset on `tank` every day ([#1097](https://github.com/tgoodyear/usnewsmap.com/issues/1097), part of [#955](https://github.com/tgoodyear/usnewsmap.com/issues/955); [ADR-0027](../../../docs/design/adr/0027-onprem-serving-behind-cloudflare.md) settled decision 3, [ADR-0022](../../../docs/design/adr/0022-onprem-cold-storage.md) decision 5). The day files can't be rebuilt and the lake holds the only copy. How the copy works, the dataset, the alert and the restore steps are in [Search serving](../../../docs/operations/search-serving.md#search-log-the-copy-on-tank); this page has the files, the choices made in building them and the install steps.

**Operational since 26 October 2026 (#1097).** The dedicated non-serving copy host has its own Arc identity, outside the shared OCR group, with only Storage Blob Data Reader on `searches`. The first run copied all 25 finalized day files (87,005 bytes); full lake-to-copy SHA256 verification passed, and an ordinary rerun copied nothing. The daily timer is enabled. Dataset creation and the independent host watchdog are tracked in [home-infra#80](https://github.com/tgoodyear/home-infra/pull/80); the setup below remains the rebuild procedure.

The restricted 10 GiB dataset is mounted, not shared or included in general guest/USB backups, with `2750` directories and `0440` day files. Daily/weekly/monthly snapshots exist. All 25 files matched a post-copy snapshot; a private restore within the dataset was hash-checked and its temporary file removed. An unrelated local user could not read the copy. No search phrases were printed or attached to these checks.

## Choices

ADR-0027 left the identity and the place the copy runs to be decided when it was built. This is what the files assume, along with two more choices: Rust over a script, and the alert.

- **Where it runs.** On a machine of its own that is enrolled in Azure Arc and can write the dataset, with nothing else running on it. The recommendation is a small unprivileged container on the host that holds `tank`, with only the dataset bind-mounted in. Any Arc host that can write the dataset works, though; no host is named in a unit. The Arc agent hands its token to every process in the `himds` group, so whatever runs on the machine can use the grant, and the grant belongs on a machine that runs nothing else. That rules out the serving container, which mounts nothing from `tank`, and the existing hosts' identities, whose other jobs would then be able to read the search log. A machine that doesn't hold `tank` would write the search text onto its own disk, or over a share, outside the dataset's restrictions. The tool refuses a destination that is not a mount point, and its partial file is in the dataset. It also refuses a `days` directory that is a symbolic link, and follows no link to a file in it: the service's state directory is writable, and a link to it would put the search text outside the dataset.
- **The identity.** The copy host's own Arc machine identity, with Storage Blob Data Reader on the `searches` container: Entra only, no SAS, no key. With `USNM_SEARCH_LOG_COPY` and `USNM_SEARCH_LOG_COPY_HOST` set, `infra/modules/searchlog-copy.bicep` grants it, and the lake opens to the node IPs (`USNM_OCR_NODE_IPS`) as it does for the other on-prem hosts. Adding the object id to `USNM_SEARCH_LOG_READERS` gives the same role, and the lake must then already be open. The role covers the whole container, `staging/` included. The tool reads only `days/`. A condition that limits the role to `days/` needs the list operation's prefix attribute, which can't be tried without a deployment, so it is left for later.
- **Rust, as a `usnm-ingest` subcommand.** `usnm-ingest searchlog-copy` uses the Blob store and the Arc credential code the pipeline already has, streams each file instead of holding it, and is tested with the store's local double. `usnm-ingest` is already built from source and installed on on-prem hosts ([Serial Set on an OCR node](../../../docs/operations/congress-pages.md#the-serial-set-on-prem)). A shell script would have needed `az`, a login and shell hashing on a machine that otherwise needs none of them. `usnm-ingest` takes `--curated`, `--reference` and a state file on every command, so the unit sets inert values for them.
- **The alert.** The command fails, and so does the unit, when a day file is still missing from the copy, or the copy differs, more than 24 hours after the lake wrote it. The counts-only status file is checked independently each hour by `home-searchlog-watch.timer` on the Proxmox host. It also checks the service result, timer enablement/activity, and a maximum status age of two days, including when the copy container is unavailable. Failures notify through root's existing Proxmox mail route, with daily reminders; no day file is opened by the watchdog. A real disabled-copy-timer drill failed the watcher, submitted one notification through the configured mail route, and recovered after the timer was restored. Dashboard integration remains separate. A missing binary fails the copy unit rather than silently skipping it.

## Files

| File                          | Runs where                    | What it does                                                                                                                                                                                                                                                               |
| ----------------------------- | ----------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `usnm-searchlog-copy.service` | The copy host, from the timer | Runs `usnm-ingest searchlog-copy` as the host's Arc identity, with write access to the dataset only. Fails if the dataset isn't mounted (the command's own check) or the binary can't run (`ExecStart`), and has no `Condition*=` or `Assert*=`, which skip a run quietly. |
| `usnm-searchlog-copy.timer`   | The copy host                 | Starts the service at 02:30 UTC every day, with up to 20 minutes' delay, and after a missed run when the host is back.                                                                                                                                                     |
| `test_units.py`               | CI and locally                | Checks that the units name flags and settings the command has, write only the dataset, keep a private `/tmp`, and run after the day file is written, and that this table lists every file.                                                                                 |

The command is `crates/usnm-ingest/src/cli/searchlog_copy.rs` and the copy logic `crates/usnm-ingest/src/searchlog_copy.rs`. The command takes named flags only; `usnm-ingest searchlog-copy --help` lists them.

## Setup

1. **The dataset.** Create `tank/azure/search-log` with the properties, quota, permissions and snapshots in [Search serving](../../../docs/operations/search-serving.md#search-log-the-copy-on-tank). The `zfs` commands live in home-infra, as for the other datasets.
2. **The host.** Create the machine and bind-mount only that dataset at `/tank/azure/search-log`. Check that the machine's backup job doesn't include the mount: a backup would carry the search text away. Install the Arc agent and enroll it under its own identity, outside the on-prem group (`USNM_ONPREM_GROUP`). Create the service user and the readers' group:

   ```sh
   sudo groupadd --system searchlog-readers
   sudo useradd --system --home-dir /nonexistent --shell /usr/sbin/nologin --groups himds usnm-searchlog
   sudo chown usnm-searchlog:searchlog-readers /tank/azure/search-log
   sudo chmod 2750 /tank/azure/search-log
   ```

   Put the operators who may read the `searches` container (`USNM_SEARCH_LOG_READERS`) in `searchlog-readers`, and nobody else.

3. **The binary.** Build `usnm-ingest` from a reviewed commit in a container, as for the Serial Set, and install it:

   ```sh
   nice -n 19 podman run --rm --cpus 3 -v "$PWD/src:/src:Z" -w /src docker.io/library/rust:1-bookworm \
     cargo build --release --locked -p usnm-ingest --bin usnm-ingest
   sudo install -m 0755 src/target/release/usnm-ingest /usr/local/bin/
   ```

4. **The grant.** Two settings and a provision. The host's Arc object id is a private setting (the settings store, never committed). `USNM_SEARCH_LOG_COPY` is a public one: `settings.sh` writes it into `infra/env/prod.json`, which has to go through a pull request, because `provision.sh` refuses an environment file with uncommitted changes (`--allow-dirty` is for experiments). The same provision opens the lake's firewall to the node IPs (`USNM_OCR_NODE_IPS`), so read the preview first.

   ```sh
   az resource show -g <Arc resource group> -n <Arc machine name> --resource-type Microsoft.HybridCompute/machines --query identity.principalId -o tsv
   scripts/azure/settings.sh --env prod --key USNM_SEARCH_LOG_COPY_HOST --value <that object id>
   ```

   Then, on a branch:

   ```sh
   scripts/azure/settings.sh --env prod --key USNM_SEARCH_LOG_COPY --value true
   ```

   Commit `infra/env/prod.json`, open the pull request and merge it. From `main`, preview and then provision:

   ```sh
   scripts/azure/provision.sh --env prod --what-if
   scripts/azure/provision.sh --env prod
   ```

   A role assignment can take a few minutes to apply.

5. **The units.** The lake's URL is the only setting:

   ```sh
   sudo install -m 0644 scripts/onprem/searchlog-copy/usnm-searchlog-copy.service scripts/onprem/searchlog-copy/usnm-searchlog-copy.timer /etc/systemd/system/
   sudo install -d -m 0755 /etc/usnm
   printf 'USNM_SEARCH_LOG_URL=%s\n' "https://<data account>.blob.core.windows.net/searches" | sudo tee /etc/usnm/searchlog-copy.env
   sudo systemctl daemon-reload
   ```

6. **The first run, by hand.** `sudo systemctl start usnm-searchlog-copy` copies every day file in the lake. Watch it with `journalctl -u usnm-searchlog-copy` (day names, sizes and counts, and checksums when a check fails, never a search), then check `/var/lib/usnm-searchlog-copy/status.json`: `ok` is true, `missing_days` and `mismatched_days` are empty, and `copied_days` equals `source_days`. Then compare every copy with the lake's bytes once, with `--verify` added to the command in a drop-in:

   ```sh
   sudo systemctl edit usnm-searchlog-copy.service   # [Service], an empty ExecStart=, then the unit's ExecStart line with --verify
   sudo systemctl start usnm-searchlog-copy.service
   sudo systemctl revert usnm-searchlog-copy.service
   ```

7. **The timer.** `sudo systemctl enable --now usnm-searchlog-copy.timer`. Watch the first few days' runs.

[ADR-0012](../../../docs/design/adr/0012-anonymous-search-log.md) and 09 §9.6 will name the second copy once it has run (the last item of #1097). They are not edited ahead of that.
