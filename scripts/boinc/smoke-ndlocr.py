#!/usr/bin/env python3
"""Exercise the actual image contract, and compare conversion with ja-ocr/alto.py."""
import argparse
from datetime import datetime
import hashlib
import json
import pathlib
import subprocess
import sys
import time
import xml.etree.ElementTree as ET


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True)
    parser.add_argument("--work", required=True, type=pathlib.Path)
    parser.add_argument("--source", type=pathlib.Path,
                        default=pathlib.Path("ja-ocr/testdata/rocky-shimpo-1945-01-01-seq-4-headline.jpg"))
    args = parser.parse_args()
    image = args.image
    work = args.work.resolve()
    source = args.source
    (work / "in").mkdir(parents=True)
    (work / "out" / "smoke").mkdir(parents=True)
    (work / "in" / "page.jpg").write_bytes(source.read_bytes())
    # The engine runs as a subordinate UID on the nodes.
    (work / "out").chmod(0o777)
    (work / "out" / "smoke").chmod(0o777)
    # The upstream fixture's known geometry, checked without a Pillow dependency here.
    dimensions = subprocess.check_output([
        "docker", "run", "--rm", "--network=none", "--entrypoint", "python3",
        "-v", f"{work / 'in'}:/in:ro", image,
        "-c", "from PIL import Image; import json; print(json.dumps(Image.open('/in/page.jpg').size))",
    ], text=True)
    width, height = json.loads(dimensions)
    request = {"type": "page", "deadline_ms": 180000, "image": {
        "dpi": 300, "dpi_source": "assumed", "height": height, "width": width,
        "path": "/in/page.jpg", "sha256": hashlib.sha256(source.read_bytes()).hexdigest(),
    }, "language": "jpn", "options": {}, "out": "/out/smoke", "profile": "default", "run_id": "smoke"}
    start = time.monotonic()
    # Match the contract host: the adapter may search /out, but can only
    # write inside the one page directory. Scratch must stay inside it.
    (work / "out").chmod(0o111)
    try:
        result = subprocess.run([
            "docker", "run", "--rm", "--network=none", "--read-only", "--cap-drop=ALL",
            "--security-opt=no-new-privileges", "--cpus=2", "--memory=4g", "--memory-swap=4g",
            "-v", f"{work / 'in'}:/in:ro", "-v", f"{work / 'out'}:/out:rw", "-i", image,
        ], input=json.dumps(request) + "\n", text=True, capture_output=True, timeout=240, check=True)
    finally:
        (work / "out").chmod(0o777)
    messages = [json.loads(line) for line in result.stdout.splitlines()]
    assert len(messages) == 2, result.stdout
    hello, answer = messages
    assert hello["type"] == "hello" and hello["engine"]["family"] == "ndlocr-lite", hello
    assert len(hello["models"]) == 4 and hello["granularity"] == "line", hello
    assert answer["status"] == "ok", answer
    alto = ET.parse(work / "out" / "smoke" / "layout.xml")
    ns = {"a": "http://www.loc.gov/standards/alto/ns-v3#"}
    words = [e.attrib["CONTENT"] for e in alto.findall(".//a:String", ns)]
    text = (work / "out" / "smoke" / "page.txt").read_text()
    assert words and text and any("\u3040" <= c <= "\u9fff" for c in text), text
    elapsed = time.monotonic() - start
    (work / "out" / "reference").mkdir()
    (work / "out" / "reference").chmod(0o777)
    # Same upstream invocation as jaocr.ndlocr, independent of the Rust adapter.
    # The resource totals include model loading; this is a headline, NOT a full page.
    probe = """
import json, resource, subprocess, time
start = time.monotonic()
subprocess.run(['python3', '/opt/ndlocr-lite/src/ocr.py', '--sourceimg', '/in/page.jpg',
                '--output', '/out/reference'], check=True)
r = resource.getrusage(resource.RUSAGE_CHILDREN)
print(json.dumps(dict(wall_seconds=time.monotonic()-start, cpu_seconds=r.ru_utime+r.ru_stime,
                     peak_rss_kib=r.ru_maxrss, onnxruntime=__import__("onnxruntime").__version__)))
"""
    reference = subprocess.check_output([
        "docker", "run", "--rm", "--network=none", "--read-only", "--cap-drop=ALL",
        "--cpus=2", "--memory=4g", "--memory-swap=4g", "--entrypoint", "python3",
        "-v", f"{work / 'in'}:/in:ro", "-v", f"{work / 'out'}:/out:rw", image, "-c", probe,
    ], text=True, timeout=240)
    costs = json.loads(reference.splitlines()[-1])
    sys.path.insert(0, str(pathlib.Path("ja-ocr").resolve()))
    import alto as converter
    import jaocr
    expected = converter.ndlocr_to_alto(
        (work / "out/reference/page.xml").read_bytes(), converter.Geometry(),
        image_url="/in/page.jpg", engine=hello["engine"]["version"], seq=1,
        ocred_at=datetime.fromisoformat(alto.findtext(".//a:processingDateTime", namespaces=ns)),
    )
    assert (work / "out/smoke/layout.xml").read_bytes() == expected, "Rust ALTO differs from ja-ocr/alto.py"
    assert text == jaocr.normalize((work / "out/reference/page.txt").read_text()), "text differs from jaocr.normalize"
    print(json.dumps({"fixture": str(source), "adapter_elapsed_seconds": round(elapsed, 3),
                      "reference_cost": costs, "characters": len(text), "lines": len(words),
                      "same_host_repeat_text_equal": True, "engine": hello["engine"], "models": hello["models"]}, ensure_ascii=False))


if __name__ == "__main__":
    main()
