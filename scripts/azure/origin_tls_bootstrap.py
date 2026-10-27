#!/usr/bin/env python3
"""Operator-only bootstrap: provision narrowly scoped DNS and Entra permissions."""

import datetime
import json
from pathlib import Path
import subprocess
import time

import origin_tls as tls

ACCOUNT = "14b06d73ae737d2512564c89dfb1e4a3"
PRINCIPAL = "4d3deafd-dffb-4bc6-98ee-484b2bf61c89"
VAULT_SCOPE = (
    f"/subscriptions/{tls.SUBSCRIPTION}/resourceGroups/home-hosts-rg"
    f"/providers/Microsoft.KeyVault/vaults/{tls.VAULT}"
)


def az(*args):
    return subprocess.check_output(["az", *args, "-o", "json"], stderr=subprocess.PIPE)


def main():
    url = f"https://{tls.VAULT}.vault.azure.net/secrets/{tls.TOKEN_SECRET}?api-version=7.4"
    try:
        existing = tls.request("GET", url, tls.azure_token("https://vault.azure.net"))
    except RuntimeError as error:
        if not str(error).endswith("HTTP 404"):
            raise
        existing = {}
    if not existing or existing.get("attributes", {}).get("exp", 0) < time.time() + 30 * 86400:
        expires = int(time.time()) + 90 * 86400
        # The IP-restricted operator token never leaves this process or reaches CI.
        result = tls.request(
            "POST", f"https://api.cloudflare.com/client/v4/accounts/{ACCOUNT}/tokens",
            tls.secret("cloudflare-api-token"), {
                "name": "usnm-prod-acme-dns01",
                "expires_on": datetime.datetime.fromtimestamp(
                    expires, datetime.timezone.utc,
                ).strftime("%Y-%m-%dT%H:%M:%SZ"),
                "policies": [{
                    "effect": "allow",
                    "resources": {f"com.cloudflare.api.account.zone.{tls.ZONE}": "*"},
                    "permission_groups": [{"id": "4755a26eedb94da69e1066d98aa820be"}],
                }],
            },
        )
        if not result.get("success"):
            raise RuntimeError("Cloudflare DNS-only token creation failed")
        token_id = result["result"]["id"]
        try:
            tls.secret(tls.TOKEN_SECRET, result["result"]["value"], expires=expires,
                       tags={"cloudflare-token-id": token_id})
        except RuntimeError:
            tls.request(
                "DELETE", f"https://api.cloudflare.com/client/v4/accounts/{ACCOUNT}/tokens/{token_id}",
                tls.secret("cloudflare-api-token"),
            )
            raise
        old_id = existing.get("tags", {}).get("cloudflare-token-id")
        if old_id:
            revoked = tls.request(
                "DELETE", f"https://api.cloudflare.com/client/v4/accounts/{ACCOUNT}/tokens/{old_id}",
                tls.secret("cloudflare-api-token"),
            )
            if not revoked.get("success"):
                raise RuntimeError("New DNS token saved but old token revocation failed")
        print("Created zone-scoped DNS-only token in Key Vault; expires after 90 days.")
    if tls.secret(tls.STATE_SECRET, missing_ok=True) is None:
        tls.secret(tls.STATE_SECRET, "uninitialized")
    for name, role in (
        (tls.TOKEN_SECRET, "Key Vault Secrets User"),
        (tls.STATE_SECRET, "Key Vault Secrets Officer"),
    ):
        az("role", "assignment", "create", "--assignee-object-id", PRINCIPAL,
           "--assignee-principal-type", "ServicePrincipal", "--role", role,
           "--scope", VAULT_SCOPE + "/secrets/" + name)
    role_name = "usnm-origin-certificate-prod"
    roles = json.loads(az("role", "definition", "list", "--name", role_name))
    if not roles:
        role = {
            "Name": role_name, "IsCustom": True,
            "Description": "Read and replace uploaded origin TLS certificates only.",
            "Actions": ["Microsoft.App/managedEnvironments/certificates/read",
                        "Microsoft.App/managedEnvironments/certificates/write"],
            "NotActions": [], "DataActions": [], "NotDataActions": [],
            "AssignableScopes": [
                f"/subscriptions/{tls.SUBSCRIPTION}/resourceGroups/rg-usnm-prod",
            ],
        }
        # Role metadata is public, not a credential.
        path = Path(".origin-tls-role.json")
        try:
            path.write_text(json.dumps(role))
            az("role", "definition", "create", "--role-definition", str(path))
        finally:
            path.unlink(missing_ok=True)
    az("role", "assignment", "create", "--assignee-object-id", PRINCIPAL,
       "--assignee-principal-type", "ServicePrincipal", "--role", role_name,
       "--scope", tls.ENVIRONMENT)
    print("Production OIDC identity has secret-level state/DNS access and certificate upload access.")


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, subprocess.CalledProcessError) as error:
        raise SystemExit(f"Origin TLS bootstrap failed: {error}") from None
