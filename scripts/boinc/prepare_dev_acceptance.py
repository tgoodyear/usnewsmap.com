#!/usr/bin/env python3
"""Write reviewable BOINC-only dev inputs; make no Azure calls or checkout config changes."""
import argparse
import json
import re
from pathlib import Path
from uuid import UUID

ROOT = Path(__file__).resolve().parents[2]
SUBSCRIPTION = "25bf257c-c94e-4d61-bba3-edc635f46602"
SERVER = "sha256:58b159d396706ebce55f9aec32904621ac7b354411d5c9bfb0f83fbb4ed046bf"
BRIDGE = "sha256:e61c5ff8d50c9e4e04bbb52dcfaf58be29806841cb9ba8d1880bd80eb36730ea"
WORKER = "ghcr.io/tgoodyear/usnm-boinc-worker@sha256:851f7f575fe84cf4d4fd238c8e280f2ac20bfe312b5dafbda2951262dbd67054"


def inputs(registry: str, operator_id: str, operator_login: str) -> dict:
    if not re.fullmatch(r"usnmboinccrdev[a-z0-9]{6}", registry):
        raise ValueError("registry must be the dedicated dev-bootstrap output, never the prod registry")
    if str(UUID(operator_id)) != operator_id or UUID(operator_id).int == 0:
        raise ValueError("operator ID must be a nonzero canonical UUID")
    if not operator_login or not operator_login.isascii() or not operator_login.isprintable():
        raise ValueError("operator login must be printable ASCII")
    settings = {
        "$schema": "./schema.json",
        "assessment": True,
        "codeSignPublicKey": "",
        "dns": {"resourceGroup": "rg-usnm-prod", "zone": "usnewsmap.com"},
        "goldBundle": False,
        "images": {
            "appver": "",
            "bridge": f"{registry}.azurecr.io/usnm-boinc-bridge@{BRIDGE}",
            "server": f"{registry}.azurecr.io/usnm-boinc-server@{SERVER}",
            "worker": WORKER,
        },
        "location": "westus2",
        "operator": {"login": operator_login, "objectId": operator_id},
        "projectUrl": "https://boinc-dev.usnewsmap.com/",
        "subscriptionId": SUBSCRIPTION,
    }
    base = json.loads(json.dumps(settings))
    base["images"].update(server="", bridge="", worker="")
    return {
        "base-settings.json": base,
        "runtime-settings.json": settings,
        "names.json": {"registry": registry, "rg": "usnm-boinc-rg-dev"},
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--registry-name", required=True)
    parser.add_argument("--operator-id", required=True)
    parser.add_argument("--operator-login", required=True)
    parser.add_argument("--output-dir", required=True, type=Path)
    args = parser.parse_args()
    try:
        files = inputs(args.registry_name, args.operator_id, args.operator_login)
        output = (ROOT / args.output_dir).resolve()
        if not output.is_relative_to(ROOT / "target") or output == ROOT / "target":
            raise ValueError("output must be beneath checkout target/, not a deployment configuration path")
        if output.exists():
            raise ValueError("output directory already exists; refuse to overwrite review inputs")
        output.mkdir(mode=0o700, parents=True)
        for name, value in files.items():
            (output / name).write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")
    except ValueError as error:
        parser.error(str(error))
    print(f"Wrote review inputs to {output}; NO resources, roles, DNS or deployed settings changed.")


if __name__ == "__main__":
    main()
