#!/usr/bin/env python3
"""Export an OpenAPI 3.0.3 Cloudflare profile from the public 3.1 document.

Checks the route inventory against a clean API checkout before replacing the
output. This is a partial request-validation profile, not an origin validator or
a whole-host allowlist. See docs/operations/search-serving.md.
"""

import argparse
import copy
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
SOURCE = "https://api.usnewsmap.com/v1/openapi.json"
ROUTER_PATH = "crates/usnm-api/src/lib.rs"
SPEC_PATH = "crates/usnm-api/src/openapi.rs"
METHODS = {"get", "head", "post", "put", "patch", "delete", "options", "trace"}
INTERNAL_POSTS = {
    "/beacon": ("beacon", "Page-view report from the site (internal)"),
    "/search-report": ("searchReport", "Edge-served search report from the site (internal)"),
}
WS = r"(?:\s|\+|%20|%09)*"
SEP = r"(?:,|%2[Cc])"
PLUS = r"(?:\+|%2[Bb])?"


def router_contract(router: str, spec: str) -> tuple[set, list, set, list]:
    """Read the explicit route table; refuse an unfamiliar layout."""
    table = re.search(r"pub fn v1_routes\b[^{]*\{\s*vec!\[(.*?)\]\s*\}", router, re.S)
    entry = r'\("([^"]+)",\s*(get|post)\([\w:]+\)\),'
    if table is None or re.sub(entry, "", table[1]).strip():
        raise ValueError("unsupported router: expected the explicit v1_routes() table")
    routes = {(path, method) for path, method in re.findall(entry, table[1])}
    prefixes = re.findall(r'\.nest\("([^"]+)",\s*v1(?:\.clone\(\))?\)', router)
    internal = re.search(r"pub const INTERNAL:.*?=\s*&\[(.*?)\];", spec, re.S)
    servers = re.search(r"pub const SERVERS:.*?=\s*&\[(.*?)\];", spec, re.S)
    if not routes or not prefixes or prefixes[0] != "/v1" or not internal or not servers:
        raise ValueError("missing route prefixes, internal-route declaration, or servers")
    return (
        routes,
        prefixes,
        set(re.findall(r'"([^"]+)"', internal[1])),
        re.findall(r'"([^"]+)"', servers[1]),
    )


def read_source(checkout: Path) -> tuple[str, str, str]:
    clean = subprocess.run(
        ["git", "-C", str(checkout), "diff", "--quiet", "HEAD", "--",
         "crates/usnm-api", "crates/usnm-core"],
        check=False,
    )
    if clean.returncode:
        raise ValueError("use a clean API checkout; existing changes will not be overwritten")
    revision = subprocess.check_output(
        ["git", "-C", str(checkout), "rev-parse", "HEAD"], text=True
    ).strip()
    router = (checkout / ROUTER_PATH).read_text(encoding="utf-8")
    spec = (checkout / SPEC_PATH).read_text(encoding="utf-8")
    for path, text in ((ROUTER_PATH, router), (SPEC_PATH, spec)):
        committed = subprocess.check_output(
            ["git", "-C", str(checkout), "show", f"{revision}:{path}"], text=True
        )
        if committed != text:
            raise ValueError(f"{path} does not match {revision}")
    return router, spec, revision


def list_pattern(item: str) -> str:
    return f"^{WS}(?:{item}{WS})?(?:{SEP}{WS}(?:{item}{WS})?)*$"


def item_regex(items: dict) -> str:
    if "enum" in items:
        return "(?:" + "|".join(re.escape(v) for v in items["enum"]) + ")"
    pattern = items.get("pattern")
    if pattern:
        return "(?:" + pattern.removeprefix("^").removesuffix("$") + ")"
    return r"[^,]*"


def int_pattern(lo: int, hi: int) -> str:
    alts = "|".join(str(n) for n in range(lo, hi + 1))
    return f"^(?:{PLUS}0*(?:{alts}))?$"


def convert_param(param: dict) -> dict:
    schema = param.get("schema", {})
    out = {
        "name": param["name"], "in": param["in"],
        "required": bool(param.get("required", False)),
    }
    if param.get("description"):
        out["description"] = param["description"]
    if param["in"] == "path":
        out["schema"] = {"type": "string"}
        if "pattern" in schema:
            out["schema"]["pattern"] = schema["pattern"]
        return out
    if param["in"] == "query" and not out["required"]:
        out["allowEmptyValue"] = True
    out["schema"] = {"type": "string"}
    kind = schema.get("type")
    if kind == "array":
        # Place IDs depend on the serving index; only the origin can check them.
        if param["name"] != "place":
            out["schema"]["pattern"] = list_pattern(item_regex(schema.get("items", {})))
    elif kind == "integer":
        out["schema"]["pattern"] = int_pattern(schema.get("minimum", 0), schema["maximum"])
    elif "enum" in schema:
        alts = "|".join(re.escape(str(v)) for v in schema["enum"])
        out["schema"]["pattern"] = f"^(?:{alts})?$"
    elif schema.get("format") == "date":
        # chrono accepts signed years and unpadded month/day, unlike OAS date.
        out["schema"]["pattern"] = (
            rf"^(?:{WS}(?:[+-]|%2[Bb])?[0-9]{{1,10}}-[0-9]{{1,2}}-[0-9]{{1,2}}{WS})?$"
        )
    elif param["name"] == "cursor":
        out["schema"]["pattern"] = r"^[A-Za-z0-9_-]*$"
    return out


def build(doc: dict, router: str, spec: str, revision: str, source: str) -> dict:
    routes, prefixes, internal, hosts = router_contract(router, spec)
    expected = {("/v1" + path, method) for path, method in routes if path not in internal}
    actual = {
        (path, method)
        for path, item in doc["paths"].items()
        for method in item
        if method in METHODS
    }
    if actual != expected:
        raise ValueError(
            f"document/router mismatch: missing={sorted(expected - actual)}, "
            f"unexpected={sorted(actual - expected)}"
        )
    if (
        internal != set(INTERNAL_POSTS)
        or {(path, method) for path, method in routes if path in internal}
        != {(path, "post") for path in internal}
    ):
        raise ValueError("internal POST routes changed; review their request contracts")
    if [server["url"] for server in doc["servers"]] != hosts:
        raise ValueError("document/router server mismatch")
    paths = {}
    for path, item in doc["paths"].items():
        for method, operation in item.items():
            if method not in METHODS:
                continue
            paths.setdefault(path, {})[method] = {
                "operationId": operation["operationId"],
                "summary": operation.get("summary", ""),
                "parameters": [convert_param(p) for p in operation.get("parameters", [])],
                "responses": {
                    "default": {"description": "See /v1/openapi.json for response schemas."}
                },
            }
    for path, (operation_id, summary) in INTERNAL_POSTS.items():
        paths["/v1" + path] = {"post": {
            "operationId": operation_id,
            "summary": summary,
            "description": (
                "Endpoint inventory only: request body validation is left to the origin. "
                "It accepts JSON as application/json or text/plain; legitimate bodies "
                "can exceed Cloudflare's Free-plan 1 KiB validation limit."
            ),
            "responses": {"default": {"description": "No content."}},
        }}
    primary = copy.deepcopy(paths)
    for index, prefix in enumerate(prefixes[1:], start=1):
        for path, item in primary.items():
            alias = copy.deepcopy(item)
            for operation in alias.values():
                operation["operationId"] += f"Alias{index}"
            paths[prefix + path.removeprefix("/v1")] = alias
    return {
        "openapi": "3.0.3",
        "info": {
            "title": doc["info"]["title"] + " (Cloudflare Schema Validation)",
            "version": doc["info"]["version"],
            "description": (
                "OpenAPI 3.0.3 request-validation profile derived from the public 3.1 document "
                f"and checked against the versioned router at {revision[:8]}. "
                "Both /v1 and /api/v1 are explicit paths. "
                "Validation is partial: origin checks remain authoritative for query grammar, "
                "cross-parameter rules, repeated/unknown keys, and POST bodies. Responses are "
                "omitted because Cloudflare does not validate them. Root health probes, "
                "static pages, implicit HEAD, and CORS OPTIONS are outside this inventory. "
                "Review Profile Analysis before enforcement; do not use this as a "
                "whole-host fallthrough allowlist."
            ),
        },
        "x-source": {
            "repository": "https://github.com/tgoodyear/usnewsmap.com",
            "referenceRevision": revision,
            "router": ROUTER_PATH,
            "routerSha256": hashlib.sha256(router.encode()).hexdigest(),
            "openapiSource": source,
            "openapiCanonicalSha256": hashlib.sha256(
                json.dumps(doc, sort_keys=True, separators=(",", ":")).encode()
            ).hexdigest(),
        },
        "servers": [{"url": host} for host in hosts],
        "paths": dict(sorted(paths.items())),
    }


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--checkout", type=Path, default=ROOT, help="clean API source checkout")
    parser.add_argument("--source", default=SOURCE, help="public OpenAPI JSON URL or local file")
    parser.add_argument("--output", type=Path, required=True, help="YAML upload file to replace")
    args = parser.parse_args(argv)
    try:
        router, spec, revision = read_source(args.checkout)
        remote = args.source.startswith(("https://", "http://"))
        if remote:
            with urllib.request.urlopen(args.source, timeout=30) as response:
                doc = json.load(response)
        else:
            doc = json.loads(Path(args.source).read_text(encoding="utf-8"))
        source = args.source if remote else Path(args.source).name
        result = build(doc, router, spec, revision, source)
        # Unit tests of conversion need only the standard library.
        import yaml

        text = yaml.safe_dump(result, sort_keys=False, width=100, allow_unicode=True)
        args.output.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.NamedTemporaryFile(
            mode="w", encoding="utf-8", dir=args.output.parent,
            prefix=f".{args.output.name}.", delete=False,
        ) as staged:
            temporary = Path(staged.name)
            try:
                staged.write(text)
                staged.flush()
                os.replace(temporary, args.output)
            finally:
                temporary.unlink(missing_ok=True)
        print(f"Wrote {args.output}")
        return 0
    except ModuleNotFoundError as error:
        print(
            f"{error}; install scripts/ops/requirements-cloudflare.txt in a virtual environment",
            file=sys.stderr,
        )
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        print(f"Cloudflare schema export failed: {error}", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
