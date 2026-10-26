#!/usr/bin/env python3
"""Run a real Japanese BOINC unit offline, then its stock validator interface."""
import argparse
import json
import pathlib
import subprocess
import xml.etree.ElementTree as ET


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--image", required=True)
    p.add_argument("--engine-image", required=True)
    p.add_argument("--work", required=True, type=pathlib.Path)
    a = p.parse_args()
    work = a.work.resolve()
    work.mkdir(parents=True)
    scan = pathlib.Path("ocr/ja-ocr/testdata/rocky-shimpo-1945-01-01-seq-4-headline.jpg").resolve()
    base = ["docker", "run", "--rm", "--network=none", "--read-only", "--cap-drop=ALL",
            "--cap-add=DAC_OVERRIDE", "--security-opt=no-new-privileges", "--cpus=1",
            "--memory=4g", "--memory-swap=4g", "-v", f"{work}:/work:rw"]
    subprocess.run(base + ["-v", f"{scan}:/scan.jpg:ro", "--entrypoint", "/usr/local/bin/usnm-ndlocr-fixture",
                          a.image, "--scan", "/scan.jpg", "--dir", "/work", "--engine-image", a.engine_image], check=True)
    subprocess.run(base + [a.image], check=True, timeout=240)
    files = ["/work/text.txt", "/work/layout.xml", "/work/result.json"]
    subprocess.run(base + [a.image, "usnm-boinc", "ndlocr", "check", "--files", *files], check=True)
    subprocess.run(base + [a.image, "usnm-boinc", "ndlocr", "compare", "--files", *files,
                          "--files2", *files], check=True)
    result = json.loads((work / "result.json").read_text())
    assert result["job"]["route"] == {"kind": "ndlocr-lite", "profile": "default"}
    assert len((work / "text.txt").read_text()) > 100
    alto = ET.parse(work / "layout.xml")
    page = alto.find(".//{http://www.loc.gov/standards/alto/ns-v3#}Page")
    assert page.attrib["PHYSICAL_IMG_NR"] == "4", page.attrib
    assert (work / "fraction_done").stat().st_mode & 0o444 == 0o444
    print(json.dumps({"elapsed_ms": result["elapsed_ms"], "route": result["job"]["route"],
                      "engine": result["job"]["engine"], "worker_validation": "passed"}))


if __name__ == "__main__":
    main()
