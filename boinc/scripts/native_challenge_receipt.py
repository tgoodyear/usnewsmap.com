#!/usr/bin/env python3
"""Turns a native build's gold-result.json into the entry boinc/project/native-build-trust.json records.

  native_challenge_receipt.py --result gold-result.json --identity identity.json \\
      --prepared boinc/project/challenges/eng-d8cd7279.prepared.json --platform arm64-apple-darwin \\
      --run 43801347852 --commit <40 hex> --artifact-sha256 <64 hex> --machine "<where it ran>" \\
      --out receipt.json [--merge boinc/project/native-build-trust.json]

run-native-challenge.sh writes the first two files. The result must be the whole prepared challenge:
the manifest the campaign's prepared.json names, every crop scored once with its reference character
count equal to prepared.json's, none skipped. The receipt holds the build's registry entry (what
check-build-trust.py validates), a per-crop edit table and a pass or fail verdict against the
threshold. It holds counts only. With --merge the entry is added to the registry file, keys sorted,
and nothing is written when the run missed the threshold.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import sys

HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("check_build_trust", HERE / "check-build-trust.py")
trust = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(trust)


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def threshold(registry: dict, manifest: str):
    """The max_cer every recorded run of this challenge was judged against, or None."""
    found = {run["max_cer"] for build in registry["builds"].values() for run in build["challenges"]
             if run["manifest_sha256"] == manifest}
    return found.pop() if len(found) == 1 else None


def receipt(result: dict, identity: dict, prepared: dict, *, platform: str, run: int, commit: str,
            artifact_sha256: str, machine: str, max_cer: float, result_sha256: str) -> dict:
    """The receipt, or ValueError if the result is not a complete run of the prepared challenge."""
    if result["manifest_sha256"] != prepared["manifest_sha256"]:
        raise ValueError("the result is for another challenge manifest")
    if result.get("limit") is not None or result["skipped"]:
        raise ValueError("a limited run or a run that skipped crops is not the challenge")
    reported = result.get("boinc") or {}
    if (reported.get("engine_image") != identity["engine_image"]
            or reported.get("engine_base") != identity["engine_base"]):
        raise ValueError("the result was not made by the build the identity names")
    expected = {crop: counts["ref_chars"] for crop, (_, counts) in prepared["crops"].items()}
    rows = {}
    for row in result["crops"]:
        if row["crop_id"] in rows:
            raise ValueError(f"crop {row['crop_id']} scored twice")
        rows[row["crop_id"]] = row
    if set(rows) != set(expected):
        raise ValueError("the scored crops are not the prepared challenge's crops")
    for crop, row in rows.items():
        if row["ref_chars"] != expected[crop]:
            raise ValueError(f"crop {crop} has another reference length than prepared.json")
        if not trust._count(row["char_edits"]):
            raise ValueError(f"crop {crop} has an invalid edit count")
    edits = sum(row["char_edits"] for row in rows.values())
    chars = sum(row["ref_chars"] for row in rows.values())
    cer = edits / chars
    evidence = (f"Run on a GitHub macos-15 runner (Apple Silicon), one thread, in a slot of soft_link files, "
                f"by native-challenge-macos.yml: {machine}; {edits} edits in {chars} characters, "
                f"{identity['elapsed_secs']} s (issue 1418)")
    entry = {
        "challenges": [{
            "char_edits": edits,
            "crops": len(rows),
            "evidence": evidence,
            "manifest_sha256": prepared["manifest_sha256"],
            "max_cer": max_cer,
            "ref_chars": chars,
        }],
        "engine_base": identity["engine_base"],
        "platform": platform,
        "source": {"artifact_sha256": artifact_sha256, "commit": commit, "run": run},
    }
    return {
        "build": {identity["engine_image"]: entry},
        "cer": cer,
        "crops": {crop: {"char_edits": rows[crop]["char_edits"], "ref_chars": rows[crop]["ref_chars"]}
                  for crop in sorted(rows)},
        "pass": cer <= max_cer,
        "result_sha256": result_sha256,
    }


def merge(registry: dict, build: dict) -> dict:
    """The registry with `build` added; an existing different entry for a digest is an error."""
    builds = dict(registry["builds"])
    for image, entry in build.items():
        if image in builds and builds[image] != entry:
            raise ValueError(f"{image} is already recorded differently")
        builds[image] = entry
    merged = {**registry, "builds": dict(sorted(builds.items()))}
    trust.validate(merged)
    return merged


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--result", type=Path, required=True)
    parser.add_argument("--identity", type=Path, required=True)
    parser.add_argument("--prepared", type=Path, required=True)
    parser.add_argument("--platform", required=True, choices=trust.PLATFORMS)
    parser.add_argument("--run", type=int, required=True)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--artifact-sha256", required=True)
    parser.add_argument("--machine", required=True)
    parser.add_argument("--max-cer", type=float)
    parser.add_argument("--registry", type=Path, default=trust.REGISTRY)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--merge", type=Path)
    args = parser.parse_args(argv)
    try:
        prepared = json.loads(args.prepared.read_text())
        max_cer = args.max_cer
        if max_cer is None:
            max_cer = threshold(trust.load(args.registry), prepared["manifest_sha256"])
        if max_cer is None:
            raise ValueError("no single recorded threshold for this challenge: pass --max-cer")
        out = receipt(json.loads(args.result.read_text()), json.loads(args.identity.read_text()), prepared,
                      platform=args.platform, run=args.run, commit=args.commit,
                      artifact_sha256=args.artifact_sha256, machine=args.machine, max_cer=max_cer,
                      result_sha256=sha256_file(args.result))
        args.out.write_text(json.dumps(out, indent=2, sort_keys=True) + "\n")
        print(f"CER {out['cer']:.5f} against {max_cer}: {'pass' if out['pass'] else 'FAIL'}")
        if args.merge is not None:
            if not out["pass"]:
                raise ValueError("the run missed the threshold: nothing merged")
            merged = merge(trust.load(args.merge), out["build"])
            args.merge.write_text(json.dumps(merged, indent=2, sort_keys=True) + "\n")
    except (OSError, ValueError, KeyError) as error:
        print(f"native_challenge_receipt: {error}", file=sys.stderr)
        return 1
    return 0 if out["pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
