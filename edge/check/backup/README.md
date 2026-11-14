# Nightly backup of check.usnewsmap.com

The answers live only in Cloudflare D1 (`usnm-check`). D1's own point-in-time restore covers recent days. This job keeps a copy of everything off Cloudflare as well, on pve-big.

Each night at 03:30 ET, `backup.sh` saves into `/tank/backups/usnm-check/<date>/`:

| File                     | What it is                                                                                                                                                       |
| ------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `dump.jsonl.gz`          | Every row of `languages`, `tasks`, `answers` and `answers_excluded`, from `GET /admin/dump`, one `{"table", "row"}` per line. Contributor ids are included, so this file is private. |
| `export-<lang>.jsonl.gz` | The public CC0 export of each language, as `GET /api/export` gives it.                                                                                           |
| `SHA256SUMS`             | Hashes of the two kinds of file above.                                                                                                                           |

`latest` points at the newest day. The script fails, and the timer shows it, if the dump is missing, doesn't parse, or has an empty table. Files are mode 600.

## Install (pve-big)

```sh
install -d -m 755 /opt/usnm-check-backup /tank/backups/usnm-check
install -m 755 backup.sh /opt/usnm-check-backup/
install -m 644 usnm-check-backup.service usnm-check-backup.timer /etc/systemd/system/
install -d -m 700 /etc/usnm-check-backup
# The admin token (Key Vault usnmtlskvprodffapzy, usnm-prod-check-admin-token), written without echoing it:
az keyvault secret show --vault-name usnmtlskvprodffapzy -n usnm-prod-check-admin-token --query value -o tsv |
  ssh root@pve-big.tjg.fyi 'umask 077; cat > /etc/usnm-check-backup/admin-token'
systemctl daemon-reload && systemctl enable --now usnm-check-backup.timer
```

## Restore

The dump holds every row, so a restore is a load into an empty database with the same schema (`../schema.sql`). For each line, insert `row` into `table`, in file order (languages, tasks, answers, then answers_excluded). The Cloudflare D1 HTTP API or `wrangler d1 execute` can run the inserts in batches.

If the admin token is rotated (`../set-secrets.sh --rotate`), write the new one to `/etc/usnm-check-backup/admin-token` too.
