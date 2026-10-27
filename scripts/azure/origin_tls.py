#!/usr/bin/env python3
"""Renew the production origin certificate using DNS-01 and Entra-authenticated storage."""

import argparse
import base64
import gzip
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

VAULT = "home-kv-bmfr6iwvyzs52"
TOKEN_SECRET = "usnm-prod-acme-cloudflare-token"
STATE_SECRET = "usnm-prod-acme-state"
ZONE = "5c88f07cb8d4b5d2857cf5bd9dc7ab19"
HOSTS = ("usnewsmap.com", "www.usnewsmap.com", "api.usnewsmap.com")
SUBSCRIPTION = "25bf257c-c94e-4d61-bba3-edc635f46602"
ENVIRONMENT = (
    f"/subscriptions/{SUBSCRIPTION}/resourceGroups/rg-usnm-prod"
    "/providers/Microsoft.App/managedEnvironments/cae-usnm-prod"
)
CERTIFICATE = ENVIRONMENT + "/certificates/usnm-public-dns01"
APP = (
    f"/subscriptions/{SUBSCRIPTION}/resourceGroups/rg-usnm-prod"
    "/providers/Microsoft.App/containerApps/ca-usnm-prod"
)
API_VERSION = "2025-01-01"


def command(*args, **kwargs):
    return subprocess.check_output(args, stderr=subprocess.PIPE, **kwargs)


def request(method, url, token, body=None, headers=None):
    encoded = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(
        url, data=encoded, method=method,
        headers={"Authorization": "Bearer " + token,
                 "Content-Type": "application/json", **(headers or {})},
    )
    try:
        with urllib.request.urlopen(req, timeout=90) as response:
            data = response.read()
            return json.loads(data) if data else {}
    except urllib.error.HTTPError as error:
        # Response bodies can contain submitted secrets. Never include them.
        raise RuntimeError(f"{method} {urllib.parse.urlsplit(url).hostname}: HTTP {error.code}") from None


def azure_token(resource):
    return command(
        "az", "account", "get-access-token", "--resource", resource,
        "--query", "accessToken", "-o", "tsv",
    ).decode().strip()


def secret(name, value=None, missing_ok=False, expires=None, tags=None):
    url = f"https://{VAULT}.vault.azure.net/secrets/{name}?api-version=7.4"
    token = azure_token("https://vault.azure.net")
    if value is not None:
        body = {"value": value}
        if expires is not None:
            body["attributes"] = {"exp": expires}
        if tags is not None:
            body["tags"] = tags
        return request("PUT", url, token, body)
    try:
        result = request("GET", url, token)
        expires = result.get("attributes", {}).get("exp")
        if name == TOKEN_SECRET and expires and expires < time.time() + 14 * 86400:
            raise RuntimeError("DNS token expires within 14 days; run origin_tls_bootstrap.py")
        return result["value"]
    except RuntimeError as error:
        if missing_ok and str(error).endswith("HTTP 404"):
            return None
        raise


def cloudflare(method, path, body=None):
    data = request(
        method, "https://api.cloudflare.com/client/v4/" + path,
        secret(TOKEN_SECRET), body,
    )
    if not data.get("success"):
        raise RuntimeError("Cloudflare rejected DNS operation")
    return data["result"]


def challenge(cleanup=False):
    domain = os.environ["CERTBOT_DOMAIN"]
    value = os.environ["CERTBOT_VALIDATION"]
    if domain not in HOSTS or not value or len(value) > 256:
        raise ValueError("Unexpected ACME challenge")
    name = "_acme-challenge." + domain
    records_path = f"zones/{ZONE}/dns_records"
    query = urllib.parse.urlencode({"type": "TXT", "name": name, "per_page": 100})
    records = cloudflare("GET", records_path + "?" + query)
    matches = [record for record in records if record["content"] == value]
    if cleanup:
        for record in matches:
            cloudflare("DELETE", records_path + "/" + record["id"])
        return
    if not matches:
        cloudflare("POST", records_path, {
            "type": "TXT", "name": name, "content": value, "ttl": 60,
            "comment": "usnm production ACME DNS-01",
        })
    # Check every authoritative server, not a local recursive cache.
    servers = command("dig", "+short", "NS", "usnewsmap.com").decode().split()
    if not servers:
        raise RuntimeError("No authoritative nameservers found")
    deadline = time.monotonic() + 300
    while time.monotonic() < deadline:
        if all(value in command("dig", "+time=3", "+tries=1", "+short", "TXT",
                                name, "@" + server).decode() for server in servers):
            time.sleep(30)
            return
        time.sleep(10)
    raise RuntimeError("ACME TXT record did not propagate")


def state_encode(config):
    files = {}
    for path in (config / "accounts").rglob("*"):
        if path.is_file():
            files[str(path.relative_to(config))] = path.read_text()
    for name in ("cert.pem", "chain.pem", "fullchain.pem", "privkey.pem"):
        files["pem/" + name] = (config / "live/usnm-origin" / name).read_text()
    value = base64.b64encode(gzip.compress(json.dumps(files).encode())).decode()
    if len(value.encode()) > 25000:
        raise RuntimeError("ACME state exceeds Key Vault secret size budget")
    return value


def state_restore(value, config):
    files = json.loads(gzip.decompress(base64.b64decode(value)))
    required = {"pem/" + name for name in
                ("cert.pem", "chain.pem", "fullchain.pem", "privkey.pem")}
    if not required.issubset(files):
        raise ValueError("Incomplete certificate state")
    for name, content in files.items():
        path = Path(name)
        if (path.is_absolute() or ".." in path.parts
                or path.parts[0] not in ("accounts", "pem")
                or not isinstance(content, str)):
            raise ValueError("Invalid certificate state path")
        target = config / path
        target.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        target.write_text(content)
        target.chmod(0o600)
    return config / "pem"


def certificate_current(pem):
    try:
        command("openssl", "x509", "-in", str(pem / "cert.pem"),
                "-noout", "-checkend", str(30 * 86400))
        for host in HOSTS:
            result = command("openssl", "x509", "-in", str(pem / "cert.pem"),
                             "-noout", "-checkhost", host).decode()
            if "does match certificate" not in result:
                return False
        return True
    except subprocess.CalledProcessError:
        return False


def arm(method, resource, body=None, headers=None):
    return request(
        method, f"https://management.azure.com{resource}?api-version={API_VERSION}",
        azure_token("https://management.azure.com/"), body, headers,
    )


def upload(pem):
    pfx = command(
        "openssl", "pkcs12", "-export", "-out", "/dev/stdout",
        "-inkey", str(pem / "privkey.pem"), "-in", str(pem / "cert.pem"),
        "-certfile", str(pem / "chain.pem"), "-passout", "pass:",
    )
    arm("PUT", CERTIFICATE, {
        "location": "eastus2",
        "properties": {"value": base64.b64encode(pfx).decode(), "password": ""},
    })
    for _ in range(60):
        certificate = arm("GET", CERTIFICATE)
        status = certificate["properties"].get("provisioningState", "Succeeded")
        if status == "Succeeded":
            return
        if status in ("Failed", "Canceled"):
            raise RuntimeError("Azure certificate provisioning failed")
        time.sleep(5)
    raise RuntimeError("Azure certificate provisioning timed out")


def bind():
    app = arm("GET", APP)
    configuration = app["properties"]["configuration"]
    domains = configuration["ingress"].get("customDomains", [])
    if not set(HOSTS).issubset({domain["name"] for domain in domains}):
        raise RuntimeError("Expected production hostname bindings are missing")
    for domain in domains:
        if domain["name"] in HOSTS:
            domain.update(certificateId=CERTIFICATE, bindingType="SniEnabled")
    # ACA does not expose an ETag. Patch only hostnames, never replay app settings.
    arm("PATCH", APP, {"properties": {"configuration": {
        "ingress": {"customDomains": domains},
    }}})
    actual = arm("GET", APP)["properties"]["configuration"]["ingress"]["customDomains"]
    if any(not any(item["name"] == host and item.get("certificateId", "").lower()
                   == CERTIFICATE.lower() for item in actual) for host in HOSTS):
        raise RuntimeError("Certificate binding verification failed")


def renew(email, no_bind=False, staging=False):
    work = Path.cwd() / ".origin-tls-work"
    work.mkdir(mode=0o700)  # Fail closed if another process owns the working directory.
    os.umask(0o077)
    try:
        config = work / "config"
        config.mkdir()
        saved = secret(STATE_SECRET, missing_ok=True)
        if saved == "uninitialized":
            saved = None
        pem = state_restore(saved, config) if saved else None
        if staging or pem is None or not certificate_current(pem):
            hook = f"{sys.executable} {Path(__file__).resolve()}"
            args = [
                "certbot", "certonly", "--non-interactive", "--agree-tos",
                "--email", email, "--manual", "--preferred-challenges", "dns",
                "--manual-auth-hook", hook + " auth",
                "--manual-cleanup-hook", hook + " cleanup",
                "--config-dir", str(config), "--work-dir", str(work / "work"),
                "--logs-dir", str(work / "logs"), "--cert-name", "usnm-origin",
                "--key-type", "rsa", "--rsa-key-size", "2048",
            ]
            if staging:
                args.append("--staging")
            for host in HOSTS:
                args.extend(["-d", host])
            # Certbot output includes only public challenge/certificate metadata.
            subprocess.run(args, check=True)
            pem = config / "live/usnm-origin"
            if staging:
                print("Staging DNS-01 validation succeeded; nothing persisted or deployed.")
                return
            if not certificate_current(pem):
                raise RuntimeError("Issued certificate failed hostname/expiry validation")
            # Persist first: a deployment failure must not discard an issued private key.
            secret(STATE_SECRET, state_encode(config))
        upload(pem)
        if not no_bind:
            bind()
        print("Production origin certificate uploaded" + (" and bound." if not no_bind else "."))
    finally:
        shutil.rmtree(work)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("renew", "auth", "cleanup"))
    parser.add_argument("--email", default="trevor@usnewsmap.com")
    parser.add_argument("--no-bind", action="store_true")
    parser.add_argument("--staging", action="store_true")
    args = parser.parse_args()
    if args.action == "renew":
        renew(args.email, args.no_bind, args.staging)
    else:
        challenge(cleanup=args.action == "cleanup")


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, ValueError, subprocess.CalledProcessError) as error:
        # Captured subprocess stderr may contain credentials; do not dump it.
        print(f"Origin TLS failed: {error}", file=sys.stderr)
        sys.exit(1)
