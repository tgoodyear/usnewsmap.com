#!/usr/bin/env python3
"""OCR the Japanese pages LoC has no text for, with NDLOCR-Lite (#128).

    python3 jaocr.py targets          list the pages to OCR (writes ocr-ja/targets.jsonl)
    python3 jaocr.py run [--limit N]  OCR the targets not done yet

Stores are blob container URLs (Entra auth via the managed identity) or local
directories, so the job runs the same way against a copy on disk:

    USNM_REFERENCE_URL  reference store: current.json, {version}/batches.json,
                        {version}/titles.json
    USNM_CURATED_URL    curated store: the batches' Parquet parts (read) and
                        ocr-ja/ (written)

Targets are pages of titles whose catalog languages include Japanese, in the
published version's batches, whose LoC text is empty, short or mostly not
words: LoC's OCR has no Japanese script, so those are the Japanese pages.

`run` claims one issue at a time by creating ocr-ja/claims/<issue>.json (a
create-only write, so replicas never take the same issue; a claim older than
JAOCR_CLAIM_HOURS is taken over). For each issue it asks loc.gov for the
page images (one API request per issue), downloads each target page's
full-resolution JPEG from LoC's IIIF service, runs NDLOCR-Lite on the issue's
pages together and writes ocr-ja/pages/<issue>.parquet: the curated page
columns with the new text, `ocr_source = usnm-ndlocr-lite` and the image URL.
An issue with a part is done. loc.gov's API is paced JAOCR_API_GAP seconds
per request and the image server JAOCR_TILE_GAP seconds, each multiplied by
JAOCR_REPLICAS so replicas together stay under LoC's limits.
"""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import os
import re
import subprocess
import sys
import tempfile
import time
import unicodedata
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from pathlib import Path

UA = "usnewsmap-ja-ocr/1.0 (+https://usnewsmap.com)"
OCR_SOURCE = "usnm-ndlocr-lite"
PREFIX = "ocr-ja"
MIN_PAGE_CHARS = 20  # crates/usnm-core/src/text.rs
# A word: Capitalized or lower case, or ALL CAPS (headlines); mixed case is OCR noise.
WORDLIKE = re.compile(r"^(?:[A-Z]?[a-z]{2,}|[A-Z]{3,15})$")
PAGE_COLUMNS = ["doc_id", "page_key", "lccn", "date", "edition", "seq", "batch", "text_status", "text"]


def log(msg: str, **fields) -> None:
    print(json.dumps({"ts": datetime.now(timezone.utc).isoformat(), "message": msg, **fields},
                     ensure_ascii=False), flush=True)


# ---------------------------------------------------------------- stores


class LocalStore:
    def __init__(self, root: str):
        self.root = Path(root)

    def read(self, path: str) -> bytes:
        return (self.root / path).read_bytes()

    def write(self, path: str, data: bytes) -> None:
        p = self.root / path
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(data)

    def create(self, path: str, data: bytes) -> bool:
        """Write only if absent; False if it exists."""
        p = self.root / path
        p.parent.mkdir(parents=True, exist_ok=True)
        try:
            with p.open("xb") as f:
                f.write(data)
            return True
        except FileExistsError:
            return False

    def list(self, prefix: str) -> list[str]:
        base = self.root / prefix
        if not base.exists():
            return []
        return [str(p.relative_to(self.root)) for p in base.rglob("*") if p.is_file()]

    def modified(self, path: str) -> datetime:
        return datetime.fromtimestamp((self.root / path).stat().st_mtime, timezone.utc)

    def exists(self, path: str) -> bool:
        return (self.root / path).is_file()


class BlobStore:
    def __init__(self, url: str):
        from azure.identity import DefaultAzureCredential
        from azure.storage.blob import ContainerClient

        self.client = ContainerClient.from_container_url(url, credential=DefaultAzureCredential())

    def read(self, path: str) -> bytes:
        return self.client.download_blob(path).readall()

    def write(self, path: str, data: bytes) -> None:
        self.client.upload_blob(path, data, overwrite=True)

    def create(self, path: str, data: bytes) -> bool:
        from azure.core.exceptions import ResourceExistsError

        try:
            self.client.upload_blob(path, data, overwrite=False)
            return True
        except ResourceExistsError:
            return False

    def list(self, prefix: str) -> list[str]:
        return [b.name for b in self.client.list_blobs(name_starts_with=prefix)]

    def modified(self, path: str) -> datetime:
        return self.client.get_blob_client(path).get_blob_properties().last_modified

    def exists(self, path: str) -> bool:
        return self.client.get_blob_client(path).exists()


def store(url: str):
    return BlobStore(url) if url.startswith("https://") else LocalStore(url)


# ---------------------------------------------------------------- targets


def japanese_lccns(titles: list[dict]) -> set[str]:
    return {t["lccn"] for t in titles if "jpn" in (t.get("languages") or [])}


def wordlike_share(text: str) -> float:
    words = text.split()
    if not words:
        return 0.0
    return sum(1 for w in words if WORDLIKE.match(w.strip(".,;:!?'\"()"))) / len(words)


def needs_ocr(text_status: str, text: str | None) -> str | None:
    """Why a Japanese title's page needs our OCR, or None if LoC's text is English."""
    if text_status != "ok":
        return text_status  # empty or short: LoC found no text
    if wordlike_share(text or "") < 0.35:
        return "garbled"
    return None


def issue_key(lccn: str, date: str, edition: int) -> str:
    return f"{lccn}_{date}_ed-{edition}"


def targets(reference, curated) -> list[dict]:
    current = json.loads(reference.read("current.json"))
    version = current["reference"]
    titles = json.loads(reference.read(f"{version}/titles.json"))
    jpn = japanese_lccns(titles)
    batches = json.loads(reference.read(f"{version}/batches.json"))
    picked = [b for b in batches if jpn & set(b["curated"]["lccns"])]
    log("listing targets", version=version, japanese_titles=len(jpn), batches=len(picked))
    import pyarrow.parquet as pq

    out = []
    for b in picked:
        for part in b["curated"]["parts"]:
            t = pq.read_table(io.BytesIO(curated.read(part)), columns=PAGE_COLUMNS)
            for r in t.to_pylist():
                if r["lccn"] not in jpn:
                    continue
                why = needs_ocr(r["text_status"], r["text"])
                if why:
                    out.append({
                        "doc_id": r["doc_id"], "page_key": r["page_key"], "lccn": r["lccn"],
                        "date": str(r["date"]), "edition": int(r["edition"]), "seq": int(r["seq"]),
                        "batch": r["batch"], "loc_text": why,
                    })
    out.sort(key=lambda r: (r["lccn"], r["date"], r["edition"], r["seq"]))
    return out


# ---------------------------------------------------------------- loc.gov


@dataclass
class Pacer:
    gap: float
    last: float = 0.0

    def wait(self) -> None:
        d = self.gap - (time.time() - self.last)
        if d > 0:
            time.sleep(d)
        self.last = time.time()


def fetch(url: str, pacer: Pacer, attempts: int = 5) -> bytes:
    for i in range(attempts):
        pacer.wait()
        try:
            req = urllib.request.Request(url, headers={"User-Agent": UA})
            with urllib.request.urlopen(req, timeout=180) as r:
                return r.read()
        except (urllib.error.URLError, TimeoutError, ConnectionError) as e:
            if isinstance(e, urllib.error.HTTPError) and e.code == 404:
                raise
            wait = 60 * (i + 1)
            if isinstance(e, urllib.error.HTTPError) and e.code == 429:
                wait = max(wait, int(e.headers.get("Retry-After") or 0))
            log("retrying", url=url[:120], error=str(e)[:200], wait=wait)
            time.sleep(wait)
    raise RuntimeError(f"giving up on {url}")


def iiif_base(jp2_url: str) -> str:
    path = jp2_url.split("/storage-services/service/", 1)[1].removesuffix(".jp2")
    return "https://tile.loc.gov/image-services/iiif/service:" + path.replace("/", ":")


def page_images(item: dict) -> dict[int, str]:
    """seq -> IIIF base URL, from loc.gov's issue JSON.

    `files` is in page (seq) order, which is not the order of the image file
    names: microfilm frames can run 0364, 0368, 0369, 0365, ... for pages 1-4.
    """
    files = (item.get("resources") or [{}])[0].get("files") or []
    out = {}
    for seq, page in enumerate(files, 1):
        jp2 = next((f.get("url", "") for f in page if f.get("mimetype") == "image/jp2"), "")
        if jp2:
            out[seq] = iiif_base(jp2)
    return out


# ---------------------------------------------------------------- OCR


def normalize(text: str) -> str:
    """NFC, no control characters, trailing spaces trimmed; newlines (columns) kept."""
    text = unicodedata.normalize("NFC", text)
    text = "".join(c for c in text if c in "\n\t " or not unicodedata.category(c).startswith("C"))
    lines = [re.sub(r"[ \t　]+", " ", ln).strip() for ln in text.split("\n")]
    return "\n".join(ln for ln in lines if ln)


def text_status(text: str) -> str:
    n = sum(1 for c in text if not c.isspace())
    return "empty" if n == 0 else "short" if len(text.strip()) < MIN_PAGE_CHARS else "ok"


def ndlocr(root: Path, images: Path, out: Path) -> None:
    # It exits 0 with "Output Directory is not found." if out doesn't exist.
    out.mkdir(parents=True, exist_ok=True)
    r = subprocess.run([sys.executable, "ocr.py", "--sourcedir", str(images), "--output", str(out)],
                       cwd=root / "src", capture_output=True, text=True)
    if r.returncode != 0 or not any(out.glob("*.txt")):
        raise RuntimeError(f"NDLOCR-Lite produced no text (exit {r.returncode}): {(r.stdout + r.stderr)[-500:]}")


def ndlocr_version(root: Path) -> str:
    """The NDLOCR-Lite commit (NDLOCR_COMMIT, set by the image), else the checkout's HEAD."""
    if os.environ.get("NDLOCR_COMMIT"):
        return os.environ["NDLOCR_COMMIT"]
    r = subprocess.run(["git", "-C", str(root), "rev-parse", "--short", "HEAD"], capture_output=True, text=True)
    return r.stdout.strip() or "unknown"


def write_part(curated, issue: str, rows: list[dict]) -> None:
    import pyarrow as pa
    import pyarrow.parquet as pq

    schema = pa.schema([
        ("doc_id", pa.string()), ("page_key", pa.string()), ("lccn", pa.string()),
        ("date", pa.date32()), ("edition", pa.int16()), ("seq", pa.int16()),
        ("batch", pa.string()), ("ocr_source", pa.string()), ("ocr_engine", pa.string()),
        ("loc_text", pa.string()), ("text_status", pa.string()), ("text", pa.large_string()),
        ("text_chars", pa.int32()), ("text_sha256", pa.binary(32)), ("image_url", pa.string()),
        ("ocred_at", pa.timestamp("us", tz="UTC")),
    ])
    for r in rows:
        r["date"] = datetime.strptime(r["date"], "%Y-%m-%d").date() if isinstance(r["date"], str) else r["date"]
    buf = io.BytesIO()
    pq.write_table(pa.Table.from_pylist(rows, schema=schema), buf, compression="zstd")
    curated.write(f"{PREFIX}/pages/{issue}.parquet", buf.getvalue())


def claim(curated, issue: str, owner: str, stale: timedelta) -> bool:
    path = f"{PREFIX}/claims/{issue}.json"
    body = json.dumps({"owner": owner, "at": datetime.now(timezone.utc).isoformat()}).encode()
    if curated.create(path, body):
        return True
    if datetime.now(timezone.utc) - curated.modified(path) > stale:
        curated.write(path, body)
        log("took over a stale claim", issue=issue)
        return True
    return False


def run(reference, curated, ndl_root: Path, limit: int | None, owner: str) -> None:
    replicas = max(1, int(os.environ.get("JAOCR_REPLICAS", "1")))
    api = Pacer(float(os.environ.get("JAOCR_API_GAP", "3.5")) * replicas)
    tile = Pacer(float(os.environ.get("JAOCR_TILE_GAP", "1.0")) * replicas)
    stale = timedelta(hours=float(os.environ.get("JAOCR_CLAIM_HOURS", "6")))
    if curated.exists(f"{PREFIX}/targets.jsonl"):
        rows = [json.loads(x) for x in curated.read(f"{PREFIX}/targets.jsonl").decode().splitlines() if x]
    else:
        rows = targets(reference, curated)
        curated.write(f"{PREFIX}/targets.jsonl", "\n".join(json.dumps(r) for r in rows).encode())
    by_issue: dict[str, list[dict]] = {}
    for r in rows:
        by_issue.setdefault(issue_key(r["lccn"], r["date"], r["edition"]), []).append(r)
    done = {Path(p).stem for p in curated.list(f"{PREFIX}/pages/")}
    todo = [i for i in sorted(by_issue) if i not in done]
    log("ocr starting", issues=len(by_issue), done=len(done), todo=len(todo),
        pages=sum(len(by_issue[i]) for i in todo), replicas=replicas)
    engine = f"ndlocr-lite {ndlocr_version(ndl_root)}"
    pages_done = 0
    for issue in todo:
        if limit is not None and pages_done >= limit:
            break
        if not claim(curated, issue, owner, stale):
            continue
        if curated.exists(f"{PREFIX}/pages/{issue}.parquet"):
            continue
        pages = by_issue[issue]
        p0 = pages[0]
        item_url = f"https://www.loc.gov/item/{p0['lccn']}/{p0['date']}/ed-{p0['edition']}/?fo=json"
        t0 = time.time()
        try:
            images = page_images(json.loads(fetch(item_url, api)))
        except urllib.error.HTTPError as e:
            log("issue not found on loc.gov", issue=issue, status=e.code)
            continue
        with tempfile.TemporaryDirectory() as tmp:
            src, out = Path(tmp, "img"), Path(tmp, "out")
            src.mkdir()
            urls = {}
            for p in pages:
                base = images.get(p["seq"])
                if not base:
                    log("no image for page", page=p["page_key"])
                    continue
                urls[p["doc_id"]] = base
                (src / f"{p['doc_id']}.jpg").write_bytes(fetch(base + "/full/full/0/default.jpg", tile))
            if not urls:
                continue
            try:
                ndlocr(ndl_root, src, out)
            except RuntimeError as e:
                # No part: the issue is retried once its claim goes stale.
                log("ocr failed", issue=issue, error=str(e)[:500])
                continue
            now = datetime.now(timezone.utc)
            rows_out = []
            for p in pages:
                f = out / f"{p['doc_id']}.txt"
                if p["doc_id"] not in urls or not f.exists():
                    continue
                text = normalize(f.read_text())
                rows_out.append({
                    **{k: p[k] for k in ("doc_id", "page_key", "lccn", "date", "edition", "seq", "batch", "loc_text")},
                    "ocr_source": OCR_SOURCE, "ocr_engine": engine,
                    "text_status": text_status(text), "text": text, "text_chars": len(text),
                    "text_sha256": hashlib.sha256(text.encode()).digest(),
                    "image_url": urls[p["doc_id"]], "ocred_at": now,
                })
        if not rows_out:
            log("ocr gave no pages", issue=issue)
            continue
        write_part(curated, issue, rows_out)
        pages_done += len(rows_out)
        log("ocr issue done", issue=issue, pages=len(rows_out), secs=round(time.time() - t0, 1),
            chars=sum(r["text_chars"] for r in rows_out))
    log("ocr finished", pages=pages_done)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("command", choices=["targets", "run"])
    ap.add_argument("--limit", type=int, help="stop after about this many pages")
    a = ap.parse_args()
    reference = store(os.environ["USNM_REFERENCE_URL"])
    curated = store(os.environ["USNM_CURATED_URL"])
    if a.command == "targets":
        rows = targets(reference, curated)
        curated.write(f"{PREFIX}/targets.jsonl", "\n".join(json.dumps(r) for r in rows).encode())
        issues = {issue_key(r["lccn"], r["date"], r["edition"]) for r in rows}
        why: dict[str, int] = {}
        for r in rows:
            why[r["loc_text"]] = why.get(r["loc_text"], 0) + 1
        log("targets written", pages=len(rows), issues=len(issues), by_loc_text=why)
    else:
        ndl = Path(os.environ.get("NDLOCR_ROOT", "/opt/ndlocr-lite"))
        owner = os.environ.get("CONTAINER_APP_REPLICA_NAME") or os.environ.get("HOSTNAME") or "local"
        run(reference, curated, ndl, a.limit, owner)


if __name__ == "__main__":
    main()
