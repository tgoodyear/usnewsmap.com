#!/usr/bin/env bash
# Runs each host script with --dry-run and checks that it prints the commands it would
# run and never prints the account key. Needs nothing but bash; run from anywhere.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
key=abcdef0123456789abcdef0123456789
printf '%s\n' "$key" > "$TMP/key"

out=$("$HERE/enroll.sh" --dry-run --project-url https://boinc.example.org/usnm/ \
  --account-key-file "$TMP/key" --ctid 9100 --storage local-lvm \
  --bridge vmbr0 --block-cidr 192.0.2.0/24)
grep -q '^+ pct create 9100' <<< "$out"
grep -q 'OUT DROP -dest 192.0.2.0/24' <<< "$out"
if grep -q "$key" <<< "$out"; then echo "enroll.sh dry run printed the key"; exit 1; fi

out=$("$HERE/run-container.sh" --dry-run --engine docker --project-url https://boinc.example.org/usnm/ \
  --account-key-file "$TMP/key" --block-cidr 192.0.2.0/24)
grep -q '^+ docker run -d --name usnm-contrib' <<< "$out"
if grep -q "$key" <<< "$out"; then echo "run-container.sh dry run printed the key"; exit 1; fi

out=$("$HERE/boinc/attach.sh" --dry-run --project-url https://boinc.example.org/usnm/ \
  --account-key-file "$TMP/key" 2>&1)
grep -q 'global_prefs_override.xml' <<< "$out"
if grep -q "$key" <<< "$out"; then echo "attach.sh dry run printed the key"; exit 1; fi

echo '{}' > "$TMP/p.json"
"$HERE/deploy-azure.sh" --phase vmss-spot --subscription 00000000-0000-0000-0000-000000000000 \
  --resource-group rg-test --parameters-file "$TMP/p.json" --what-if --dry-run \
  | grep -q '^+ az deployment group what-if'
echo "dry runs ok"
