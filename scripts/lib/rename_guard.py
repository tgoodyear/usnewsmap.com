"""Refuse deletion of stack-managed durable resources (ADR-0017).

stdin contains two JSON values: the stack's resources array, then the full
deployment what-if response. Nothing here calls Azure or writes files.
"""

import argparse
import json
import re
import sys

PROTECTED_TYPES = {
    "microsoft.storage/storageaccounts",
    "microsoft.documentdb/databaseaccounts",
    "microsoft.containerregistry/registries",
    "microsoft.operationalinsights/workspaces",
    "microsoft.keyvault/vaults",
    "microsoft.app/managedenvironments",
    "microsoft.network/virtualnetworks",
    "microsoft.network/dnszones",
    "microsoft.network/privatednszones",
}
RETAINED_CHANGES = {"Create", "Modify", "NoChange", "Deploy"}


def resource_type(resource_id):
    if re.fullmatch(r"/subscriptions/[^/]+/resourcegroups/[^/]+/?", resource_id.lower()):
        return "microsoft.resources/resourcegroups"
    parts = resource_id.lower().rstrip("/").split("/providers/")[-1].split("/")
    if len(parts) < 3 or len(parts) % 2 != 1:
        raise ValueError(f"invalid resource id: {resource_id}")
    return parts[0] + "/" + "/".join(parts[1::2])


def check(resources, plan, allowed=()):
    if not isinstance(resources, list):
        raise ValueError("stack resources must be an array")
    if not isinstance(plan, dict) or not isinstance(plan.get("changes"), list):
        raise ValueError("what-if has no changes array")
    if plan.get("status", "Succeeded") != "Succeeded" or plan.get("error"):
        raise ValueError("what-if did not succeed")
    # A short-circuited nested deployment can omit its actual resources.
    # Missing protected IDs are therefore unsafe, even without an Ignore row.
    changes = {}
    for row in plan["changes"]:
        if not isinstance(row, dict) or not isinstance(row.get("resourceId"), str):
            raise ValueError("what-if change has no resourceId")
        rid = row["resourceId"].rstrip("/").lower()
        if rid in changes:
            raise ValueError(f"duplicate what-if resource: {rid}")
        changes[rid] = row.get("changeType")
    blocked = []
    allowed = {name.lower() for name in allowed}
    for row in resources:
        if not isinstance(row, dict) or not isinstance(row.get("id"), str):
            raise ValueError("stack resource has no id")
        rid = row["id"].rstrip("/")
        if resource_type(rid) not in PROTECTED_TYPES:
            continue
        change = changes.get(rid.lower())
        if change in RETAINED_CHANGES:
            continue
        name = rid.rsplit("/", 1)[-1]
        # An override authorizes a known deletion, never an incomplete plan.
        if change in {"Ignore", "Delete"} and name.lower() in allowed:
            continue
        reason = change or "missing from what-if (possibly a short-circuited deployment)"
        blocked.append(f"{rid}: {reason}")
    return blocked


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--allow-rename", action="append", default=[])
    args = parser.parse_args()
    if any(not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_. -]*", name) for name in args.allow_rename):
        parser.error("--allow-rename requires one exact resource name, not a pattern or id")
    try:
        text = sys.stdin.read()
        decoder = json.JSONDecoder()
        resources, end = decoder.raw_decode(text.lstrip())
        plan = json.loads(text.lstrip()[end:])
        blocked = check(resources, plan, args.allow_rename)
    except (ValueError, TypeError) as error:
        print(f"error: rename guard cannot establish a safe plan: {error}", file=sys.stderr)
        return 1
    if blocked:
        print("error: rename guard refuses to remove protected stack resources:", file=sys.stderr)
        for row in blocked:
            print(f"  {row}", file=sys.stderr)
        print("For an intentional Ignore/Delete only, inspect the data/migration plan and pass "
              "--allow-rename=<exact-old-name> once per resource. Missing/unsupported plans "
              "must be resolved, not overridden.", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
