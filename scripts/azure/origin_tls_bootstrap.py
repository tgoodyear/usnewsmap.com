#!/usr/bin/env python3
"""Operator-only bootstrap: provision narrowly scoped DNS and Entra permissions."""

import argparse
import datetime
import json
from pathlib import Path
import subprocess
import time
import uuid

if __package__:
    from . import origin_tls as tls
else:
    import origin_tls as tls

ACCOUNT = "14b06d73ae737d2512564c89dfb1e4a3"
PRINCIPAL = "4d3deafd-dffb-4bc6-98ee-484b2bf61c89"
# CT230's Arc machine identity, usnm-boinc-arcs-prod (#1357): it reads only the
# BOINC LAN deploy secret.
BOINC_HOST_PRINCIPAL = "0dd45a40-23bb-45e0-b205-b704bd10f3c6"
VAULT_SCOPE = (
    f"/subscriptions/{tls.SUBSCRIPTION}/resourceGroups/rg-usnm-prod"
    f"/providers/Microsoft.KeyVault/vaults/{tls.VAULT}"
)
WORKSPACE = (
    f"/subscriptions/{tls.SUBSCRIPTION}/resourceGroups/rg-usnm-prod"
    "/providers/Microsoft.OperationalInsights/workspaces/log-usnm-prod"
)


def az(*args):
    subscription = [] if args[0] == "ad" else ["--subscription", tls.SUBSCRIPTION]
    return subprocess.check_output(
        ["az", *args, *subscription, "-o", "json"],
        stderr=subprocess.PIPE,
    )


def provision_vault():
    operator = json.loads(az("ad", "signed-in-user", "show", "--query", "id"))
    role_id = "b86a8fe4-44ce-4948-aee5-eccb2c155cd7"  # Key Vault Secrets Officer
    management_token = tls.azure_token("https://management.azure.com/")
    vault_url = f"https://management.azure.com{VAULT_SCOPE}?api-version=2023-07-01"
    try:
        tls.request("GET", vault_url, management_token)
        exists = True
    except RuntimeError as error:
        if not str(error).endswith("HTTP 404"):
            raise
        exists = False
    template = {
        "$schema": "https://schema.management.azure.com/schemas/2019-04-01/deploymentTemplate.json#",
        "contentVersion": "1.0.0.0",
        "resources": [{
            "type": "Microsoft.KeyVault/vaults", "apiVersion": "2023-07-01",
            "name": tls.VAULT, "location": "eastus2",
            "tags": {"purpose": "production-origin-dns01", "managed-by": "origin_tls_bootstrap.py"},
            "properties": {
                "tenantId": "a203c0c8-bd4b-4679-b826-4de746bee5da",
                "sku": {"family": "A", "name": "standard"},
                "enableRbacAuthorization": True, "enableSoftDelete": True,
                "enablePurgeProtection": True, "softDeleteRetentionInDays": 90,
                "accessPolicies": [], "publicNetworkAccess": "Disabled",
                "networkAcls": {"bypass": "None", "defaultAction": "Allow",
                                "ipRules": [], "virtualNetworkRules": []},
            },
        }],
    }
    path = Path(".origin-tls-vault-" + uuid.uuid4().hex + ".json")
    try:
        if not exists:
            path.write_text(json.dumps(template))
            az("deployment", "group", "create", "--resource-group", "rg-usnm-prod",
               "--name", "origin-tls-vault", "--template-file", str(path),
               "--only-show-errors")
    finally:
        path.unlink(missing_ok=True)
    tls.request(
        "PUT",
        f"https://management.azure.com{VAULT_SCOPE}/providers/"
        "Microsoft.Insights/diagnosticSettings/usnm-origin-tls"
        "?api-version=2021-05-01-preview",
        management_token,
        {"properties": {"workspaceId": WORKSPACE,
                        "logs": [{"category": "AuditEvent", "enabled": True}]}},
    )
    # Never weaken the group policy or exempt any other vault. GitHub-hosted
    # runners need this endpoint; Entra secret-level RBAC remains enforced.
    exemption_url = (
        f"https://management.azure.com{VAULT_SCOPE}/providers/"
        "Microsoft.Authorization/policyExemptions/usnm-origin-tls-github"
        "?api-version=2022-07-01-preview"
    )
    tls.request("PUT", exemption_url, management_token, {"properties": {
        "displayName": "Production DNS-01 vault: GitHub OIDC network access",
        "description": "Only ACME state and zone-DNS token; secret-level Entra RBAC, no access to home vault.",
        "exemptionCategory": "Mitigated",
        "policyAssignmentId": (
            f"/subscriptions/{tls.SUBSCRIPTION}/resourceGroups/rg-usnm-prod"
            "/providers/Microsoft.Authorization/policyAssignments/usnm-audit-data-public-network"
        ),
    }})
    tls.request("PATCH", vault_url, management_token, {"properties": {
        "publicNetworkAccess": "Enabled", "enableRbacAuthorization": True,
        "networkAcls": {"bypass": "None", "defaultAction": "Allow",
                        "ipRules": [], "virtualNetworkRules": []},
    }})
    assignments = json.loads(az("role", "assignment", "list",
                               "--assignee", operator, "--scope", VAULT_SCOPE))
    if not any(item["roleDefinitionId"].endswith("/" + role_id) for item in assignments):
        az("role", "assignment", "create", "--assignee-object-id", operator,
           "--assignee-principal-type", "User", "--role", role_id, "--scope", VAULT_SCOPE)


def operator_token():
    return tls.secret("cloudflare-api-token", vault=tls.OPERATOR_VAULT)


def read_existing_token(timeout=300):
    url = f"https://{tls.VAULT}.vault.azure.net/secrets/{tls.TOKEN_SECRET}?api-version=7.4"
    deadline = time.monotonic() + timeout
    while True:
        try:
            return tls.request("GET", url, tls.azure_token("https://vault.azure.net"))
        except RuntimeError as error:
            if str(error).endswith("HTTP 404"):
                return {}
            if not str(error).endswith("HTTP 403") or time.monotonic() >= deadline:
                raise
            time.sleep(min(10, max(0, deadline - time.monotonic())))


def rotate_dns_token(force=False):
    existing = read_existing_token()
    if force and not existing.get("tags", {}).get("cloudflare-token-id"):
        raise RuntimeError("Explicit rotation requires an existing tracked DNS token")
    if force or not existing or existing.get("attributes", {}).get("exp", 0) < time.time() + 30 * 86400:
        expires = int(time.time()) + 90 * 86400
        # The IP-restricted operator token never leaves this process or reaches CI.
        result = tls.request(
            "POST", f"https://api.cloudflare.com/client/v4/accounts/{ACCOUNT}/tokens",
            operator_token(), {
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
                operator_token(),
            )
            raise
        old_id = existing.get("tags", {}).get("cloudflare-token-id")
        if old_id:
            revoked = tls.request(
                "DELETE", f"https://api.cloudflare.com/client/v4/accounts/{ACCOUNT}/tokens/{old_id}",
                operator_token(),
            )
            if not revoked.get("success"):
                raise RuntimeError("New DNS token saved but old token revocation failed")
        print("Created zone-scoped DNS-only token in Key Vault; expires after 90 days.")


def bootstrap_credentials():
    rotate_dns_token()
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


def bootstrap_boinc_lan():
    spec = tls.TARGETS["boinc-lan"]
    # Secret-scoped assignments need the secrets to exist; the host's sync
    # ignores the placeholder until the first certificate is published.
    for name in (spec["state"], spec["deploy"]):
        if tls.secret(name, missing_ok=True) is None:
            tls.secret(name, "uninitialized")
    for name, principal, role in (
        (spec["state"], PRINCIPAL, "Key Vault Secrets Officer"),
        (spec["deploy"], PRINCIPAL, "Key Vault Secrets Officer"),
        (spec["deploy"], BOINC_HOST_PRINCIPAL, "Key Vault Secrets User"),
    ):
        az("role", "assignment", "create", "--assignee-object-id", principal,
           "--assignee-principal-type", "ServicePrincipal", "--role", role,
           "--scope", VAULT_SCOPE + "/secrets/" + name)
    print("BOINC LAN certificate: OIDC identity writes its state and deploy secrets; "
          "the BOINC host reads only the deploy secret.")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--rotate-dns-token", action="store_true",
        help="Replace the tracked DNS token now, without provisioning or changing Entra grants",
    )
    parser.add_argument(
        "--boinc-lan", action="store_true",
        help="Only create the BOINC LAN certificate secrets and their secret-scoped grants",
    )
    args = parser.parse_args(argv)
    if args.rotate_dns_token:
        rotate_dns_token(force=True)
        return
    if args.boinc_lan:
        bootstrap_boinc_lan()
        return
    provision_vault()
    bootstrap_credentials()
    bootstrap_boinc_lan()


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, subprocess.CalledProcessError) as error:
        raise SystemExit(f"Origin TLS bootstrap failed: {error}") from None
