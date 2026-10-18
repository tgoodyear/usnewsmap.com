#!/usr/bin/env python3
"""Build or verify the bounded GovInfo Serial Set pilot (#814)."""
import argparse
import hashlib
import json
import re
import shutil
import subprocess
from pathlib import Path, PurePosixPath

ROOT = Path(__file__).resolve().parents[2]
PILOT = ROOT / "ocr/bench/serialset-pilot"
SELECTION = PILOT / "selection.json"
FORMAT = "usnm-serialset-ocr-pilot-selection/1"
SUBSET = "serialset/verified-3"
INPUT_PREFIXES = ("images/", "references/", "source/alto/", "outputs/")
INPUT_FILES = {"selection.json", "tesseract-runtime.json", "LICENSES/govinfo/NOTICE.txt"}
HEX64 = re.compile(r"[0-9a-f]{64}\Z")


def sha(data):
    return hashlib.sha256(data).hexdigest()


def read_json(path):
    return json.loads(path.read_text(encoding="utf-8"))


def safe_path(name):
    path = PurePosixPath(name)
    return bool(name) and not path.is_absolute() and all(part not in ("", ".", "..") for part in path.parts)


def files_under(root):
    rows = []
    for path in root.rglob("*"):
        if path.is_symlink():
            raise ValueError(f"symlink is not allowed: {path.relative_to(root)}")
        if path.is_file() and path.name != "SHA256SUMS":
            rows.append(path.relative_to(root).as_posix())
    return sorted(rows)


def input_paths():
    return [name for name in files_under(PILOT)
            if name in INPUT_FILES or name.startswith(INPUT_PREFIXES)]


def jpeg_dimensions(data, name):
    if not data.startswith(b"\xff\xd8"):
        raise ValueError(f"{name}: not a JPEG")
    i = 2
    sof = {0xC0, 0xC1, 0xC2, 0xC3, 0xC5, 0xC6, 0xC7, 0xC9, 0xCA, 0xCB, 0xCD, 0xCE, 0xCF}
    while i + 4 <= len(data):
        if data[i] != 0xFF:
            i += 1
            continue
        while i < len(data) and data[i] == 0xFF:
            i += 1
        if i >= len(data):
            break
        marker = data[i]
        i += 1
        if marker in (0xD8, 0xD9) or 0xD0 <= marker <= 0xD7 or marker == 0x01:
            continue
        if i + 2 > len(data):
            break
        length = int.from_bytes(data[i:i + 2], "big")
        if length < 2 or i + length > len(data):
            break
        if marker in sof:
            height = int.from_bytes(data[i + 3:i + 5], "big")
            width = int.from_bytes(data[i + 5:i + 7], "big")
            if width and height:
                return width, height
            break
        i += length
    raise ValueError(f"{name}: JPEG dimensions not found")


def file_ref(relative):
    path = PILOT / relative
    data = path.read_bytes()
    return {"bytes": len(data), "path": relative, "sha256": sha(data)}


def expected_manifest(selection, inputs_sha):
    verified = [p for p in selection["pages"] if p["annotation_status"] == "human-verified"]
    if len({p["source_page_id"] for p in selection["pages"]}) != len(selection["pages"]):
        raise ValueError("page selection is not page-disjoint")
    samples = []
    for page in verified:
        doc = next(d for d in selection["documents"] if d["package_id"] == page["package_id"])
        alto = next(a for a in doc["alto_pages"] if a["pdf_page"] == page["pdf_page"])
        image = file_ref(page["image"])
        image_data = (PILOT / page["image"]).read_bytes()
        width, height = jpeg_dimensions(image_data, page["image"])
        reference = file_ref(page["reference"])
        samples.append({
            "damage": {"status": "unannotated", "observations": page["observed_visual_features"]},
            "id": page["id"],
            "image": image,
            "image_height": height,
            "image_width": width,
            "languages": ["eng"],
            "legacy": {"status": "new-serialset-pilot", "selection": "selection.json"},
            "mix": "monolingual",
            "provenance": {
                "alto_page_id": page["alto_page_id"],
                "alto_sha256": alto["sha256"],
                "alto_url": alto["url"],
                "date_issued": doc["date_issued"],
                "image_generation": "full-page Poppler pdftoppm 26.06.0 at 200 dpi, JPEG quality 92",
                "package_id": doc["package_id"],
                "page_url": doc["pdf_url"],
                "pdf_page": page["pdf_page"],
                "pdf_sha256": doc["pdf_sha256"],
                "retrieved_at": selection["selected_at"],
                "source_detail_url": f"https://www.govinfo.gov/app/details/{doc['package_id']}",
                "source_ocr_engine": page["source_ocr_engine"],
                "source_scan_attribution": "GPO and Library of Congress digitization partnership",
                "visual_features": page["observed_visual_features"],
                "layout": page["layout"],
            },
            "reference": reference,
            "reference_status": "human-verified",
            "scripts": ["Latn"],
            "source": "govinfo",
            "source_page_id": page["source_page_id"],
            "split": "public-eval",
            "typeface": "serif print; typeface subtype unclassified",
            "unit": "full-page",
        })
    samples.sort(key=lambda row: row["id"])
    members = [sample["id"] for sample in samples]
    member_lines = "".join(
        f"{sample['id']} {sample['image']['sha256']} {sample['reference']['sha256']}\n"
        for sample in samples
    )
    coverage = coverage_data(selection, samples)
    return {
        "coverage": {
            "sample_pages": coverage["sample_pages"],
            "human_verified_pages": coverage["human_verified_pages"],
            "annotation_gap_pages": coverage["annotation_gap_pages"],
            "source_documents": coverage["source_documents"],
            "page_disjoint": coverage["page_disjoint"],
            "limitations": coverage["limitations"],
        },
        "examples": [],
        "format": "usnm-ocr-bench",
        "inputs_sha256": inputs_sha,
        "limitations": [
            "A purposive four-page pilot, not a representative estimate of all GovInfo Serial Set OCR.",
            "Only three pages have human-verified references; the fourth remains an explicit annotation gap.",
            "No formal damage severity labels, scoring masks, or full-page reading-order adjudication beyond the pilot policy.",
            "All scored pages are public evaluation data, not a held-out test set.",
        ],
        "release": "v0.0.0",
        "rules": ["legacy-ws-v1", "raw-v1"],
        "samples": samples,
        "schema": 1,
        "sources": {
            "govinfo": {
                "annotation_status": "human-verified",
                "attribution": "U.S. Congressional Serial Set pages served by GovInfo; scanned by the Library of Congress and made available through the GPO partnership.",
                "image_license": "U.S. federal government work; public domain in the United States.",
                "languages": ["eng"],
                "reference_license": "Transcription of the same U.S. federal government public-domain pages; no third-party annotation set copied.",
                "scripts": ["Latn"],
                "unit": "full-page",
            }
        },
        "spec_sha256": sha(SELECTION.read_bytes()),
        "subsets": {
            SUBSET: {
                "count": len(members),
                "description": "All and only the full-page samples whose references were checked against their page images.",
                "digest": sha(member_lines.encode("utf-8")),
                "members": members,
                "pages": len({sample["source_page_id"] for sample in samples}),
                "recipe": {"selection": "selection.json", "status": "human-verified", "deterministic": True},
            }
        },
    }


def coverage_data(selection, verified_samples=None):
    verified_samples = verified_samples or []
    page_rows = []
    for page in selection["pages"]:
        image = file_ref(page["image"])
        dims = jpeg_dimensions((PILOT / page["image"]).read_bytes(), page["image"])
        row = {
            "id": page["id"],
            "source_page_id": page["source_page_id"],
            "package_id": page["package_id"],
            "pdf_page": page["pdf_page"],
            "image": image,
            "image_width": dims[0],
            "image_height": dims[1],
            "annotation_status": page["annotation_status"],
            "layout": page["layout"],
            "observed_visual_features": page["observed_visual_features"],
            "reference": file_ref(page["reference"]) if page["reference"] else None,
            "gap_reason": page.get("gap_reason"),
        }
        page_rows.append(row)
    verified = sum(p["annotation_status"] == "human-verified" for p in selection["pages"])
    gap = len(page_rows) - verified
    return {
        "format": "usnm-serialset-ocr-pilot-coverage/1",
        "selection": "selection.json",
        "retrieved_at": selection["selected_at"],
        "sample_pages": len(page_rows),
        "human_verified_pages": verified,
        "annotation_gap_pages": gap,
        "source_documents": len({p["package_id"] for p in selection["pages"]}),
        "page_disjoint": len({p["source_page_id"] for p in selection["pages"]}) == len(page_rows),
        "damage_taxonomy_status": "not formally annotated; observed visual features are descriptive only",
        "pages": page_rows,
        "strata": {
            "single-column prose": {"pages": 2, "human_verified": 2, "needs_human_verification": 0},
            "comparative or multi-column tables": {"pages": 2, "human_verified": 1, "needs_human_verification": 1},
            "pages with visible scan irregularities": {"pages": 2, "human_verified": 1, "needs_human_verification": 1},
        },
        "limitations": [
            "Selection is purposive and very small; counts are not estimates of corpus prevalence.",
            "One dense comparative table page has no reference and is excluded from accuracy scoring.",
            "Observed scan features are not mapped to severity labels; no damage claim is made for clean text regions.",
            "All 3 scored pages come from 2 distinct serial numbers and are public evaluation data.",
        ],
    }


def canonical(value):
    return json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n"


def expected_inputs_sums():
    rows = []
    for name in input_paths():
        rows.append(f"{sha((PILOT / name).read_bytes())}  {name}\n")
    return "".join(rows)


def expected_sums():
    return "".join(f"{sha((PILOT / name).read_bytes())}  {name}\n" for name in files_under(PILOT))


def build():
    selection = read_json(SELECTION)
    if selection.get("format") != FORMAT:
        raise ValueError("unexpected selection format")
    inputs = expected_inputs_sums()
    (PILOT / "inputs.sha256").write_text(inputs, encoding="utf-8")
    input_digest = sha(inputs.encode("utf-8"))
    manifest = expected_manifest(selection, input_digest)
    coverage = coverage_data(selection)
    (PILOT / "manifest.json").write_text(canonical(manifest), encoding="utf-8")
    (PILOT / "coverage.json").write_text(canonical(coverage), encoding="utf-8")
    (PILOT / "SHA256SUMS").write_text(expected_sums(), encoding="utf-8")
    verify(quiet=True)


def verify(quiet=False):
    selection = read_json(SELECTION)
    if selection.get("format") != FORMAT:
        raise ValueError("unexpected selection format")
    for doc in selection["documents"]:
        if not HEX64.fullmatch(doc["pdf_sha256"]):
            raise ValueError(f"{doc['package_id']}: invalid PDF digest")
        for page in doc["alto_pages"]:
            data = (PILOT / page["fixture_path"]).read_bytes()
            if sha(data) != page["sha256"]:
                raise ValueError(f"ALTO checksum mismatch: {page['fixture_path']}")
        mods = ROOT / doc["mods_fixture"]
        if sha(mods.read_bytes()) != doc["mods_fixture_sha256"]:
            raise ValueError(f"MODS fixture checksum mismatch: {mods}")
    for page in selection["pages"]:
        if page["annotation_status"] == "human-verified":
            if not page["reference"] or not (PILOT / page["reference"]).is_file():
                raise ValueError(f"{page['id']}: verified reference is missing")
        elif page["annotation_status"] == "needs-human-verification":
            if page["reference"] is not None:
                raise ValueError(f"{page['id']}: unverified page must not have a reference")
        else:
            raise ValueError(f"{page['id']}: unknown annotation status")
        if not (PILOT / page["image"]).is_file():
            raise ValueError(f"{page['id']}: image is missing")
    expected_ids = sorted(p["id"] for p in selection["pages"]
                          if p["annotation_status"] == "human-verified")
    for name in ("source-ocr.jsonl", "tesseract-5.5.1.jsonl"):
        rows = [json.loads(line) for line in (PILOT / "outputs" / name).read_text(encoding="utf-8").splitlines()]
        ids = [row.get("id") for row in rows]
        if ids != expected_ids:
            raise ValueError(f"outputs/{name}: IDs differ from verified subset")
        if any(not isinstance(row.get("text"), str) for row in rows):
            raise ValueError(f"outputs/{name}: output text is missing")
    gap = read_json(PILOT / "outputs/annotation-gap.json")
    pending = [p for p in selection["pages"] if p["annotation_status"] == "needs-human-verification"]
    if len(pending) != 1 or gap.get("id") != pending[0]["id"] or gap.get("reference") is not None:
        raise ValueError("annotation-gap output must bind to the one unverified page and have no reference")
    inputs = expected_inputs_sums()
    if (PILOT / "inputs.sha256").read_text(encoding="utf-8") != inputs:
        raise ValueError("inputs.sha256 differs from the selected inputs")
    input_digest = sha(inputs.encode("utf-8"))
    if (PILOT / "manifest.json").read_text(encoding="utf-8") != canonical(expected_manifest(selection, input_digest)):
        raise ValueError("manifest.json is stale; run with --build")
    if (PILOT / "coverage.json").read_text(encoding="utf-8") != canonical(coverage_data(selection)):
        raise ValueError("coverage.json is stale; run with --build")
    sums = (PILOT / "SHA256SUMS").read_text(encoding="utf-8")
    if sums != expected_sums():
        raise ValueError("SHA256SUMS differs from the pilot files")
    if not quiet:
        digest = sha(sums.encode("utf-8"))
        print(f"verified {len(selection['pages'])} pages; {len(expected_ids)} references; 1 annotation gap")
        print(f"serialset pilot digest: {digest}")


def score():
    verify(quiet=True)
    manifest = read_json(PILOT / "manifest.json")
    scratch = ROOT / "work/serialset-814/score-run"
    if scratch.exists():
        raise ValueError(f"score work path already exists: {scratch}")
    harness = scratch / f"usnm-ocr-bench-{manifest['release']}"
    results = scratch / "results"
    scratch.mkdir(parents=True)
    try:
        results.mkdir()
        shutil.copytree(PILOT, harness, dirs_exist_ok=True)
        for engine, output in (
            ("source-ocr", "outputs/source-ocr.jsonl"),
            ("tesseract-5.5.1", "outputs/tesseract-5.5.1.jsonl"),
        ):
            for rule in ("legacy-ws-v1", "raw-v1"):
                name = f"{engine}-{rule}.json"
                command = [
                    "cargo", "run", "--quiet", "-p", "usnm-ocr-survey", "--",
                    "bench-score", "--release", str(harness), "--subset", SUBSET,
                    "--outputs", str(PILOT / output), "--rule", rule,
                    "--report", str(results / name),
                ]
                subprocess.run(command, cwd=ROOT, check=True)
        for report in sorted(results.glob("*.json")):
            (PILOT / "reports" / report.name).write_bytes(report.read_bytes())
        build()
    finally:
        shutil.rmtree(scratch)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument("--build", action="store_true", help="regenerate manifest, coverage and checksums")
    action.add_argument("--verify", action="store_true", help="verify selection, fixtures, outputs and checksums")
    action.add_argument("--score", action="store_true", help="run #738 CER rules and save checked score reports")
    args = parser.parse_args()
    try:
        if args.build:
            build()
        elif args.score:
            score()
        else:
            verify()
    except (OSError, ValueError, KeyError, json.JSONDecodeError, subprocess.CalledProcessError) as exc:
        parser.error(str(exc))


if __name__ == "__main__":
    main()
