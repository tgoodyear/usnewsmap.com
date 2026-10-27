#!/usr/bin/env python3
"""Stage ADR-0027 cache rules without changing DNS, TLS or unrelated zone rules."""

import argparse
import json
from pathlib import Path
import re
import subprocess
import urllib.error
import urllib.request

HERE = Path(__file__).resolve().parent
FIELDS = ("ref", "description", "expression", "action", "action_parameters", "enabled")


def definition(rule):
    return {key: rule[key] for key in FIELDS}


def apply(request, zone, desired):
    entrypoint = f"/zones/{zone}/rulesets/phases/http_request_cache_settings/entrypoint"
    before = request("GET", entrypoint)
    ruleset = f"/zones/{zone}/rulesets/{before['id']}"
    refs = [rule["ref"] for rule in desired]
    existing = {}
    for rule in before["rules"]:
        if rule.get("ref") in refs:
            if rule["ref"] in existing:
                raise ValueError(f"duplicate managed ref: {rule['ref']}")
            existing[rule["ref"]] = rule
    # Rule-level edits preserve OCR dashboard, basemap and other operators' rules.
    # Move the managed rules to the end in order; the defensive bypass is last.
    for rule in desired:
        old = existing.get(rule["ref"])
        payload = rule | {"position": {"after": ""}}
        if old is None:
            request("POST", ruleset + "/rules", payload)
        else:
            request("PATCH", ruleset + "/rules/" + old["id"], payload)
    after = request("GET", entrypoint)
    managed = [definition(rule) for rule in after["rules"] if rule.get("ref") in refs]
    if managed != desired or after["rules"][-1].get("ref") != refs[-1]:
        raise ValueError("managed rules or their order differ after read-back")
    unrelated = lambda rules: [rule for rule in rules if rule.get("ref") not in refs]
    # Rule versions/timestamps can change on a ruleset update; content must not.
    def content(rules):
        return [{k: v for k, v in rule.items() if k not in ("version", "last_updated")}
                for rule in unrelated(rules)]
    if content(before["rules"]) != content(after["rules"]):
        raise ValueError("unrelated cache rules changed; inspect the zone before proceeding")
    return {"before": before, "after": after}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--zone-id", required=True)
    parser.add_argument("--vault")
    parser.add_argument("--token-secret")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    if not re.fullmatch(r"[0-9a-f]{32}", args.zone_id):
        parser.error("--zone-id must be 32 lowercase hex digits")
    desired = json.loads((HERE / "cache-rules.json").read_text())
    if args.dry_run:
        print(json.dumps(desired, indent=2))
        return
    if not args.vault or not args.token_secret:
        parser.error("--vault and --token-secret are required except with --dry-run")
    token = subprocess.check_output([
        "az", "keyvault", "secret", "show", "--vault-name", args.vault,
        "--name", args.token_secret, "--query", "value", "-o", "tsv",
        "--only-show-errors",
    ], text=True).strip()
    if not token:
        raise ValueError("Key Vault returned an empty Cloudflare token")

    def request(method, path, body=None):
        data = None if body is None else json.dumps(body).encode()
        req = urllib.request.Request(
            "https://api.cloudflare.com/client/v4" + path, data=data, method=method,
            headers={"Authorization": "Bearer " + token, "Content-Type": "application/json"},
        )
        try:
            with urllib.request.urlopen(req, timeout=60) as response:
                result = json.load(response)
        except urllib.error.HTTPError as error:
            raise RuntimeError(f"Cloudflare {method} {path}: HTTP {error.code}") from None
        if not result.get("success"):
            raise RuntimeError(f"Cloudflare {method} {path}: {result.get('errors')}")
        return result["result"]

    print(json.dumps(apply(request, args.zone_id, desired), indent=2))


if __name__ == "__main__":
    main()
