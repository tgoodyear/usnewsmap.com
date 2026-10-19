#!/usr/bin/env python3
"""
Distributed full-corpus (3,011 batches / 23.77M pages) staging + submission + rolling collection/purge
runner for usnm_quality 1.3 across all 5 native BOINC platforms.
Runs inside usnm-boinc-quality-caj-prod (Python 3.11 + Pillow 11.3.0 + /usr/local/bin/usnm-boinc).
"""
import fcntl
import gc
import glob
import hashlib
import io
import json
import os
import re
import shutil
import subprocess
import sys
import threading
import time
import traceback
import urllib.error
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET
import zipfile
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

sys.path.insert(0, "/usr/local/lib/usnm-boinc/gold/py")

DUCKDB_WHL_URL = "https://files.pythonhosted.org/packages/50/52/6e6f5b5b07841cec334ca6b98f2e02b7bb54ab3b99c49aa3a161cc0b4b37/duckdb-1.2.1-cp311-cp311-manylinux_2_17_x86_64.manylinux2014_x86_64.whl"

SCANS_SAS = os.environ.get("SCANS_SAS", "")
# No SAS: without one, curated reads use the job's managed identity (Storage Blob Data Reader).
CURATED_SAS = os.environ.get("CURATED_SAS", "")
HANDOFF_SAS = os.environ.get("HANDOFF_SAS", "")
STORAGE_BEARER_TOKEN = os.environ.get("STORAGE_BEARER_TOKEN", "")
_MI_TOKEN_CACHE = {"token": "", "expires_at": 0.0}


def get_storage_bearer_token(for_curated: bool = False) -> str:
    now = time.time()
    if _MI_TOKEN_CACHE["token"] and now < _MI_TOKEN_CACHE["expires_at"]:
        return _MI_TOKEN_CACHE["token"]
    endpoint = os.environ.get("IDENTITY_ENDPOINT")
    header = os.environ.get("IDENTITY_HEADER")
    client_id = os.environ.get("AZURE_CLIENT_ID")
    if endpoint and header:
        q = f"{endpoint}?api-version=2019-08-01&resource=https://storage.azure.com/"
        if client_id:
            q += f"&client_id={urllib.parse.quote(client_id)}"
        req = urllib.request.Request(q, headers={"X-IDENTITY-HEADER": header})
        data = json.loads(urllib.request.urlopen(req, timeout=15).read())
        tok = data.get("access_token", "")
        if tok:
            _MI_TOKEN_CACHE["token"] = tok
            _MI_TOKEN_CACHE["expires_at"] = now + 1800.0
            return tok
    return STORAGE_BEARER_TOKEN


def azure_blob_url_and_headers(url: str, extra_headers: dict | None = None) -> tuple[str, dict]:
    headers = {"User-Agent": "usnewsmap-quality/1.3"}
    if extra_headers:
        headers.update(extra_headers)
    parsed = urllib.parse.urlsplit(url)
    host = (parsed.hostname or "").lower()
    if host == "blob.core.windows.net" or host.endswith(".blob.core.windows.net"):
        base, sep, qs = url.partition("?")
        qs = qs.strip("&?")
        if "sv=" in qs:
            return f"{base}?{qs}", headers
        clean_url = f"{base}?{qs}" if qs else base
        tok = get_storage_bearer_token(for_curated=(host == "stusnmdffapzy.blob.core.windows.net"))
        if tok:
            headers["Authorization"] = "Bearer " + tok
            headers["x-ms-version"] = "2021-08-06"
        return clean_url, headers
    return url, headers

SCANS_BASE = "https://usnmscansstprodffapzy.blob.core.windows.net/scans"
CURATED_BASE = "https://stusnmdffapzy.blob.core.windows.net/curated"
HANDOFF_BASE = "https://stusnmoffapzy.blob.core.windows.net/boinc-handoff"
LOC_STORAGE_ROOT = "https://tile.loc.gov/storage-services/service/ndnp/"

PROJ = "/project/usnmocr"
SPOOL_DIR = "/project/usnmocr/results/0"
SEEN_DIR = "/project/quality/seen"
WAVE9_ROOT = "/project/quality/v1-wave9"
UNITS_DIR = f"{WAVE9_ROOT}/units"
COLLECTED_DIR = f"{WAVE9_ROOT}/collected"
STAGED_META_DIR = f"{WAVE9_ROOT}/staged_meta"
BATCH_INDEX_DIR = f"{WAVE9_ROOT}/batch_index"
LOCKS_DIR = f"{WAVE9_ROOT}/locks"
LOCAL_LOCKS_DIR = "/tmp/w9_local_locks"
FW_DIR = f"{WAVE9_ROOT}/function-words"
PLACES_PATH = f"{WAVE9_ROOT}/places.txt"
SHARED_MANIFEST = f"{WAVE9_ROOT}/wave9_manifest.parquet"

ENGINE_IMAGE = "sha256:3f599aefa49334ef6d12cf9d4a3b294fe7638877d587526b94bba0048741a402"
READERS = "loc-hist,as,ours-ndlocr,ours-paddle,ours-surya,ours-tesseract"
BUNDLE_SIZE = int(os.environ.get("BUNDLE_SIZE", "200"))
NUM_SHARDS = int(os.environ.get("NUM_SHARDS", "8"))
MAX_IN_FLIGHT_BUNDLES = max(500, int(os.environ.get("MAX_IN_FLIGHT_BUNDLES", "500")))
MIN_FREE_GB = float(os.environ.get("MIN_FREE_GB", "35.0"))
LOC_GAP_SECS = 5.0

ALL_LANGS = [
    "ces", "dan", "deu", "eng", "fin", "fra", "haw", "hrv", "hun", "ita",
    "lav", "lit", "nor", "pol", "ron", "rus", "slv", "spa", "srp", "swe", "yid",
]
LANG_ALIASES = {"ger": "deu", "fre": "fra", "cze": "ces", "rum": "ron"}

DOUBLE_COLS = [
    "est_cer", "est_cer_raw", "completeness", "precision",
    "findable_content", "findable_places",
    "cer_vs_loc", "wer_vs_loc", "dice_vs_loc", "word_ratio_vs_loc",
    "damage_rate", "function_share", "garbage_share",
    "hat_residual", "legibility_ceiling",
    "skew_deg", "blur", "contrast", "noise", "bleed_through", "ink_density",
    "otsu_local_diff", "bg_tile_std", "est_line_height_px",
    "vertical_stroke_ratio", "fraktur_score", "gutter_clarity",
    "ink_word_coverage", "numeric_share", "short_line_share",
    "line_height_px", "mean_word_conf",
]

BIGINT_COLS = [
    "head_words", "near_words", "known_words", "consensus_words",
    "width", "height", "blocks", "lines", "line_words", "columns", "img_columns",
]

BOOL_COLS = ["hat_determined"]


def bootstrap_runtime():
    os.makedirs(LOCAL_LOCKS_DIR, exist_ok=True)
    bf = open(f"{LOCAL_LOCKS_DIR}/bootstrap.lock", "w")
    fcntl.flock(bf, fcntl.LOCK_EX)
    try:
        done_marker = f"{LOCAL_LOCKS_DIR}/bootstrap.done"
        if not os.path.exists(done_marker):
            subprocess.run(
                ["/bin/bash", "-c", ". /usr/local/lib/usnm-boinc/lib.sh && fetch_secrets && render_config"],
                check=True,
            )
            duck_dir = "/tmp/duckdb_pkg"
            if not os.path.exists(f"{duck_dir}/duckdb"):
                os.makedirs(duck_dir, exist_ok=True)
                whl_path = "/tmp/duckdb.whl"
                req = urllib.request.urlopen(DUCKDB_WHL_URL, timeout=90)
                with open(whl_path, "wb") as f:
                    f.write(req.read())
                with zipfile.ZipFile(whl_path, "r") as zf:
                    zf.extractall(duck_dir)
                os.remove(whl_path)
            Path(done_marker).write_text("ok\n")
    finally:
        fcntl.flock(bf, fcntl.LOCK_UN)
        bf.close()
    sys.path.insert(0, "/tmp/duckdb_pkg")


def put_blob(blob_path, data_bytes, content_type="application/octet-stream"):
    raw_url = f"{HANDOFF_BASE}/{blob_path}?{HANDOFF_SAS}"
    url, headers = azure_blob_url_and_headers(
        raw_url,
        {
            "x-ms-blob-type": "BlockBlob",
            "Content-Type": content_type,
            "Content-Length": str(len(data_bytes)),
        },
    )
    req = urllib.request.Request(url, data=data_bytes, method="PUT", headers=headers)
    urllib.request.urlopen(req, timeout=120).read()


def touch_shard_heartbeat(shard_idx):
    try:
        hb = f"{LOCKS_DIR}/shard-{shard_idx}.lock/heartbeat"
        if os.path.exists(os.path.dirname(hb)):
            Path(hb).write_text(f"{time.time()}\n")
    except Exception:
        pass


def put_status(name, obj):
    try:
        if isinstance(obj, dict) and "shard" in obj:
            touch_shard_heartbeat(obj["shard"])
        data = json.dumps(obj, indent=2).encode("utf-8")
        put_blob(f"quality/v1-wave9/status/{name}", data, "application/json")
    except Exception as e:
        print(f"put_status({name}) warning: {e}", file=sys.stderr, flush=True)


def run_mysql(sql):
    pw = open("/dev/shm/boinc/db-runtime").read().strip()
    host = os.environ["DB_HOST"]
    user = os.environ.get("DB_RUNTIME_USER", "usnmboincrun")
    db = os.environ.get("DB_NAME", "usnmocr")
    cmd = [
        "mysql",
        f"-h{host}",
        f"-u{user}",
        f"-p{pw}",
        "--ssl",
        "--ssl-verify-server-cert",
        "--ssl-ca=/usr/local/share/usnm/mysql-ca.pem",
        "-N",
        "-B",
        db,
        "-e",
        sql,
    ]
    return subprocess.check_output(cmd, text=True, stderr=subprocess.DEVNULL)


def download_bytes(url, retries=4):
    for attempt in range(retries):
        try:
            clean_url, headers = azure_blob_url_and_headers(url)
            req = urllib.request.Request(clean_url, headers=headers)
            return urllib.request.urlopen(req, timeout=90).read()
        except urllib.error.HTTPError as e:
            if e.code == 401:
                _MI_TOKEN_CACHE["expires_at"] = 0.0
            if attempt == retries - 1:
                raise
            time.sleep(1 + attempt)
        except Exception:
            if attempt == retries - 1:
                raise
            time.sleep(1 + attempt)


JPEG_CACHE_PREFIX = os.environ.get("JPEG_CACHE_PREFIX", "quality-jpeg/l-q75-r1/")


def cached_page(blob_name):
    """The on-prem page JPEG cache (#850): this recipe's output for the master, or None."""
    if not JPEG_CACHE_PREFIX or not blob_name:
        return None
    raw_url = f"{HANDOFF_BASE}/{JPEG_CACHE_PREFIX}{urllib.parse.quote(blob_name)}.jpg?{HANDOFF_SAS}"
    for attempt in range(2):
        try:
            clean_url, headers = azure_blob_url_and_headers(raw_url)
            req = urllib.request.Request(clean_url, headers=headers)
            with urllib.request.urlopen(req, timeout=90) as resp:
                data = resp.read()
                want = resp.headers.get("x-ms-meta-sha256", "")
            if not want or hashlib.sha256(data).hexdigest() == want:
                return data
            return None
        except urllib.error.HTTPError as e:
            if e.code == 404:
                return None
            if e.code == 401:
                _MI_TOKEN_CACHE["expires_at"] = 0.0
        except Exception:
            pass
        time.sleep(1)
    return None


def download_blob(url, dst, retries=4):
    try:
        data = download_bytes(url, retries=retries)
        tmp = f"{dst}.tmp.{os.getpid()}"
        with open(tmp, "wb") as f:
            f.write(data)
        os.replace(tmp, dst)
        return True
    except Exception:
        return False


def loc_paced_download(url):
    """
    Polite LoC tile.loc.gov downloader: enforces a global >= 5.0s gap across all shards
    using /tmp/wave9_local_locks/loc_pacer.ts, with 1 attempt and HTML block check.
    """
    os.makedirs(LOCAL_LOCKS_DIR, exist_ok=True)
    pacer_lock = open(f"{LOCAL_LOCKS_DIR}/loc_pacer.lock", "w")
    pacer_ts = f"{LOCAL_LOCKS_DIR}/loc_pacer.ts"
    fcntl.flock(pacer_lock, fcntl.LOCK_EX)
    try:
        last_ts = 0.0
        if os.path.exists(pacer_ts):
            try:
                last_ts = float(Path(pacer_ts).read_text().strip())
            except Exception:
                last_ts = 0.0
        wait = LOC_GAP_SECS - (time.time() - last_ts)
        if wait > 0:
            time.sleep(wait)
        Path(pacer_ts).write_text(f"{time.time()}\n")
        req = urllib.request.Request(url, headers={"User-Agent": "usnewsmap-quality/1.3"})
        data = urllib.request.urlopen(req, timeout=90).read()
        if data[:64].lstrip().lower().startswith((b"<!doctype html", b"<html")):
            raise RuntimeError(f"LoC returned HTML challenge for {url}")
        return data
    finally:
        fcntl.flock(pacer_lock, fcntl.LOCK_UN)
        pacer_lock.close()


def list_blobs(base_url, sas, prefix, delimiter=None):
    out = []
    marker = ""
    while True:
        q = f"restype=container&comp=list&prefix={urllib.parse.quote(prefix)}"
        if delimiter:
            q += f"&delimiter={urllib.parse.quote(delimiter)}"
        if marker:
            q += f"&marker={urllib.parse.quote(marker)}"
        url = f"{base_url}?{q}&{sas}"
        xml_bytes = download_bytes(url)
        root = ET.fromstring(xml_bytes)
        if delimiter:
            for p in root.findall(".//BlobPrefix/Name"):
                if p.text:
                    out.append(p.text)
        else:
            for b in root.findall(".//Blob/Name"):
                if b.text:
                    out.append(b.text)
        nm = root.findtext(".//NextMarker")
        if not nm:
            break
        marker = nm
    return out


def dir_hier_path(physical):
    return subprocess.check_output(
        [f"{PROJ}/bin/dir_hier_path", physical], text=True, cwd=PROJ
    ).strip()


def spool_ready(wu):
    return all(
        os.path.exists(f"{SPOOL_DIR}/{wu}__file_{fn}")
        for fn in ("result.json", "confusions.json", "diag.txt")
    )


def spool_gave_up(wu):
    return (
        os.path.exists(f"{SPOOL_DIR}/{wu}_error")
        or os.path.exists(f"{SPOOL_DIR}/{wu}_no_output_files")
    )


def purge_unit_downloads(jid):
    reclaimed_bytes = 0
    reclaimed_files = 0
    try:
        run_mysql(f"""
            UPDATE result r
            JOIN workunit w ON r.workunitid = w.id
            SET r.server_state = 5, r.outcome = 5
            WHERE w.name = 'usnm_quality_{jid}' AND r.server_state = 2;
            UPDATE workunit
            SET assimilate_state = 2, transition_time = 2147483647
            WHERE name = 'usnm_quality_{jid}';
        """)
    except Exception:
        pass
    for suffix in ("_page_jpg", "_loc_xml", "_texts_json", "_quality_json"):
        try:
            p = dir_hier_path(f"{jid}{suffix}")
            if os.path.exists(p):
                sz = os.path.getsize(p)
                os.remove(p)
                reclaimed_bytes += sz
                reclaimed_files += 1
        except Exception:
            pass
    return reclaimed_bytes, reclaimed_files


def free_assimilated_downloads():
    reclaimed_bytes = 0
    reclaimed_files = 0
    if os.path.exists(UNITS_DIR):
        for bdir in sorted(glob.glob(f"{UNITS_DIR}/bundle-*")):
            udir = Path(bdir)
            if (udir / "collected").exists():
                continue
            wu = None
            if (udir / "submitted").exists():
                wu = (udir / "submitted").read_text().strip()
            elif (udir / "quality.json").exists():
                try:
                    jid0 = json.loads((udir / "quality.json").read_text()).get("id")
                    if jid0:
                        wu = f"usnm_quality_{jid0}"
                except Exception:
                    pass
            if not wu or not (spool_ready(wu) or spool_gave_up(wu)):
                continue
            jid = wu.removeprefix("usnm_quality_")
            try:
                run_mysql(f"""
                    UPDATE result r
                    JOIN workunit w ON r.workunitid = w.id
                    SET r.server_state = 5, r.outcome = 5
                    WHERE w.name = '{wu}' AND r.server_state = 2;
                    UPDATE workunit
                    SET assimilate_state = 2, transition_time = 2147483647
                    WHERE name = '{wu}';
                """)
            except Exception:
                pass
            for suffix in ("_page_jpg", "_loc_xml"):
                try:
                    p = dir_hier_path(f"{jid}{suffix}")
                    if os.path.exists(p):
                        sz = os.path.getsize(p)
                        os.remove(p)
                        reclaimed_bytes += sz
                        reclaimed_files += 1
                except Exception:
                    pass
    return reclaimed_bytes, reclaimed_files


def query_mysql_workunits():
    """
    Returns dict mapping workunit_name -> (assimilate_state, error_mask, active_results) for all usnm_quality WUs.
    """
    mysql_wus = {}
    try:
        out = run_mysql("""
            SELECT
                w.name,
                w.assimilate_state,
                w.error_mask,
                COALESCE(SUM(CASE WHEN r.server_state IN (1, 2, 4) THEN 1 ELSE 0 END), 0)
                + (CASE WHEN w.assimilate_state = 0 AND w.error_mask = 0 AND COUNT(r.id) = 0 THEN 1 ELSE 0 END) AS active_results
            FROM workunit w
            LEFT JOIN result r ON r.workunitid = w.id
            WHERE w.name LIKE 'usnm_quality_%'
            GROUP BY w.id, w.name, w.assimilate_state, w.error_mask;
        """).strip().splitlines()
        for line in out:
            parts = line.split("\t")
            if len(parts) >= 4 and parts[1].strip().isdigit() and parts[2].strip().isdigit():
                act = int(parts[3].strip()) if parts[3].strip().lstrip("-").isdigit() else 0
                mysql_wus[parts[0].strip()] = (int(parts[1].strip()), int(parts[2].strip()), act)
    except Exception as e:
        print(f"query_mysql_workunits warning: {e}", file=sys.stderr, flush=True)
    return mysql_wus


def rename_colliding_workunit(wu):
    run_mysql(f"""
        UPDATE result r
        JOIN workunit w ON r.workunitid = w.id
        SET r.name = CONCAT(r.name, '_old_', r.id),
            r.server_state = CASE WHEN r.server_state = 2 THEN 5 ELSE r.server_state END,
            r.outcome = CASE WHEN r.server_state = 2 THEN 5 ELSE r.outcome END
        WHERE w.name = '{wu}';
        UPDATE workunit
        SET name = CONCAT(name, '_old_', id), assimilate_state = 2, error_mask = 16, transition_time = 2147483647
        WHERE name = '{wu}';
    """)


def qwen_job_identity(job):
    inp = {
        "bytes": int(job["input"]["bytes"]),
        "sha256": str(job["input"]["sha256"]),
        "source": str(job["input"]["source"]),
    }
    crops = [
        {
            "box": [int(x) for x in c["box"]],
            "id": str(c["id"]),
            "loc_text": str(c["loc_text"]),
            "stratum": str(c["stratum"]),
        }
        for c in job["crops"]
    ]
    eng = {
        "image": str(job["engine"]["image"]),
        "llama": str(job["engine"]["llama"]),
        "models": [
            {
                "file": str(m["file"]),
                "min_gpu_mb": int(m["min_gpu_mb"]),
                "mmproj": str(m["mmproj"]),
                "name": str(m["name"]),
            }
            for m in job["engine"]["models"]
        ],
        "weights": str(job["engine"]["weights"]),
    }
    tup = [
        int(job["v"]),
        str(job["doc_id"]),
        str(job["group"]),
        str(job["language"]),
        inp,
        crops,
        eng,
        int(job["prompt_version"]),
        int(job["gpu_timeout_secs"]),
        int(job["cpu_timeout_secs"]),
        int(job["max_tokens_per_1000px2"]),
    ]
    if job.get("pages"):
        tup.append(job["pages"])
    raw = json.dumps(tup, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    return hashlib.sha256(raw).hexdigest()


def ungate_and_fill_gpu_queues():
    info = {}
    try:
        # 1. Deprecate docker_* app versions & cancel stuck docker (batch 12, 18) Unsent tasks so
        #    non-Docker clients stop receiving "Some projects require Docker" warnings and feeder slots free up.
        run_mysql("""
            UPDATE app_version SET deprecated = 1 WHERE plan_class LIKE 'docker_%';
            UPDATE result r
            JOIN workunit w ON r.workunitid = w.id
            JOIN app a ON w.appid = a.id
            SET r.server_state = 5, r.outcome = 5
            WHERE a.name = 'usnm_qwen' AND w.batch IN (2, 12, 18) AND r.server_state = 2;
            UPDATE workunit w
            JOIN app a ON w.appid = a.id
            SET w.assimilate_state = 2, w.error_mask = 16, w.transition_time = 2147483647
            WHERE a.name = 'usnm_qwen' AND w.batch IN (2, 12, 18) AND w.assimilate_state = 0;
        """)
        # 2. Ungate GPU plan classes in /project/usnmocr/plan_class_spec.xml
        pcs_path = Path(f"{PROJ}/plan_class_spec.xml")
        if pcs_path.exists():
            tree = ET.parse(pcs_path)
            root = tree.getroot()
            ungated = []
            for pc in root.findall("plan_class"):
                name = (pc.findtext("name") or "").strip()
                if name in (
                    "nvidia_vulkan_8b",
                    "intel_vulkan_8b",
                    "amd_vulkan_8b",
                    "apple_gpu_8b",
                    "nvidia_cuda_8b",
                ):
                    removed = False
                    for tag in ("min_wu_id", "max_wu_id"):
                        for el in list(pc.findall(tag)):
                            pc.remove(el)
                            removed = True
                    if removed:
                        ungated.append(name)
            if ungated:
                tmp_pcs = Path(f"{PROJ}/plan_class_spec.xml.tmp")
                tree.write(tmp_pcs, encoding="utf-8", xml_declaration=False)
                os.replace(tmp_pcs, pcs_path)
            info["ungated_plan_classes"] = ungated

        # 3. Inspect WU 17099's engine object (or use canonical native engine)
        native_engine = {
            "image": "sha256:247c131a599403645c4c96782c0556dc27f27a5d991c764b4b9565182a8c71aa",
            "llama": "sha256:d962a8228c42c84583150e809d654f9ec995cdf9f4e02e37365293642210a309",
            "models": [
                {
                    "file": "Qwen3VL-8B-Instruct-Q4_K_M.gguf",
                    "min_gpu_mb": 6000,
                    "mmproj": "mmproj-Qwen3VL-8B-Instruct-Q8_0.gguf",
                    "name": "qwen3-vl-8b-q4km",
                },
                {
                    "file": "Qwen3-VL-2B-Instruct-Q8_0.gguf",
                    "min_gpu_mb": 0,
                    "mmproj": "mmproj-Qwen3-VL-2B-Instruct-Q8_0.gguf",
                    "name": "qwen3-vl-2b-q8",
                },
            ],
            "weights": "sha256:969266d1c24656977e06a6efacae04deac6d5093c77227fe55ac28975035cddd",
        }
        w17099_out = run_mysql("SELECT name FROM workunit WHERE id IN (17099, 17100) LIMIT 1;").strip()
        if w17099_out.startswith("usnm_qwen_"):
            j17099 = w17099_out.removeprefix("usnm_qwen_")
            p17099 = dir_hier_path(f"{j17099}_qwen_json")
            if os.path.exists(p17099):
                d17099 = json.loads(Path(p17099).read_text())
                if "engine" in d17099:
                    native_engine = d17099["engine"]
                    info["engine_source"] = f"wu_17099:{j17099[:12]}"

        # 4. Find legacy batch=8 usnm_qwen WUs (id < 17099) that have Unsent results,
        #    re-stage them with native_engine, submit to create_work --batch 8, and cancel old WUs
        legacy_rows = run_mysql("""
            SELECT DISTINCT w.id, w.name
            FROM workunit w
            JOIN result r ON r.workunitid = w.id
            JOIN app a ON w.appid = a.id
            WHERE a.name = 'usnm_qwen' AND w.batch = 8 AND w.id < 17099 AND r.server_state = 2
            ORDER BY w.id ASC;
        """).strip().splitlines()
        restaged_qwen = 0
        cancelled_legacy_qwen = 0
        for line in legacy_rows:
            parts = line.split("\t")
            if len(parts) < 2 or not parts[0].strip().isdigit():
                continue
            old_wid = int(parts[0].strip())
            old_wu = parts[1].strip()
            old_jid = old_wu.removeprefix("usnm_qwen_")
            old_jpg = dir_hier_path(f"{old_jid}_page_jpg")
            old_json = dir_hier_path(f"{old_jid}_qwen_json")
            if os.path.exists(old_jpg) and os.path.exists(old_json):
                try:
                    job = json.loads(Path(old_json).read_text())
                    if qwen_job_identity(job) == job.get("id"):
                        job["engine"] = native_engine
                        job["gpu_timeout_secs"] = max(3600, int(job.get("gpu_timeout_secs", 3600)))
                        new_jid = qwen_job_identity(job)
                        job["id"] = new_jid
                        new_wu = f"usnm_qwen_{new_jid}"
                        new_jpg = dir_hier_path(f"{new_jid}_page_jpg")
                        new_json = dir_hier_path(f"{new_jid}_qwen_json")
                        os.makedirs(os.path.dirname(new_jpg), exist_ok=True)
                        os.makedirs(os.path.dirname(new_json), exist_ok=True)
                        if not os.path.exists(new_jpg):
                            shutil.copyfile(old_jpg, new_jpg)
                        Path(new_json).write_text(json.dumps(job, indent=2))
                        exists_new = run_mysql(f"SELECT id FROM workunit WHERE name = '{new_wu}' LIMIT 1;").strip()
                        if not exists_new:
                            cw_qwen = [
                                f"{PROJ}/bin/create_work",
                                "--appname", "usnm_qwen",
                                "--wu_name", new_wu,
                                "--wu_template", "templates/usnm_qwen_8b_in",
                                "--result_template", "templates/usnm_qwen_out",
                                "--batch", "8",
                                "--min_quorum", "2",
                                "--target_nresults", "2",
                                "--max_error_results", "3",
                                "--max_total_results", "4",
                                "--max_success_results", "3",
                                "--rsc_fpops_est", "1e13",
                                "--rsc_fpops_bound", "1e16",
                                "--rsc_memory_bound", "6442450944",
                                "--rsc_disk_bound", "19327352832",
                                "--delay_bound", "259200",
                                f"{new_jid}_page_jpg",
                                f"{new_jid}_qwen_json",
                                "usnm_qwen_67d1659bfe71b89d50b45a4ad1a9e5b997e5bb16ce5da66a6a6167abd569e9e2_Qwen3VL-8B-Instruct-Q4_K_M.gguf",
                                "usnm_qwen_c6ba85508d82f42590e6eb77d5340369ab6fecf107a7561d809523d8aa5f3bfd_mmproj-Qwen3VL-8B-Instruct-Q8_0.gguf",
                            ]
                            subprocess.run(cw_qwen, cwd=PROJ, check=True)
                            restaged_qwen += 1
                except Exception as e:
                    info["restage_last_error"] = str(e)
            run_mysql(f"""
                UPDATE result SET server_state = 5, outcome = 5 WHERE workunitid = {old_wid} AND server_state = 2;
                UPDATE workunit SET assimilate_state = 2, error_mask = 16, transition_time = 2147483647 WHERE id = {old_wid};
            """)
            cancelled_legacy_qwen += 1
        info["restaged_qwen_8b_wus"] = restaged_qwen
        info["cancelled_legacy_qwen_wus"] = cancelled_legacy_qwen
    except Exception as e:
        info["gpu_error"] = str(e)
    return info


def repair_and_reclaim_download_space():
    try:
        if os.path.exists(SPOOL_DIR):
            os.listdir(SPOOL_DIR)
    except Exception:
        pass
    try:
        run_mysql("""
            UPDATE result r
            JOIN workunit w ON r.workunitid = w.id
            SET r.name = CONCAT(r.name, '_old_', r.id)
            WHERE w.name LIKE 'usnm_quality_%_old_%'
              AND r.name NOT LIKE '%_old_%';
            UPDATE result r
            JOIN workunit w ON r.workunitid = w.id
            SET r.server_state = 5, r.outcome = 5
            WHERE (w.name LIKE 'usnm_quality_%_old_%' OR w.error_mask != 0 OR w.assimilate_state != 0)
              AND r.server_state = 2;
            UPDATE workunit
            SET transition_time = 2147483647
            WHERE name LIKE 'usnm_quality_%_old_%'
              AND transition_time != 2147483647;
        """)
    except Exception as e:
        print(f"old wu cleanup warning: {e}", file=sys.stderr, flush=True)
    reclaimed_bytes, reclaimed_files = free_assimilated_downloads()

    # Check all active (Unsent or In-progress) usnm_quality WUs in MySQL and cancel any whose download files are missing on disk
    # so transitioner never spawns _2/_3 tiebreakers for missing-file WUs when an in-progress result fails.
    cancelled_missing_dl = 0
    try:
        unsent_rows = run_mysql("""
            SELECT DISTINCT w.id, w.name
            FROM workunit w
            LEFT JOIN result r ON r.workunitid = w.id
            WHERE w.name LIKE 'usnm_quality_%'
              AND w.name NOT LIKE '%_old_%'
              AND ((w.assimilate_state = 0 AND w.error_mask = 0) OR r.server_state = 2);
        """).strip().splitlines()
        for line in unsent_rows:
            parts = line.split("\t")
            if len(parts) < 2 or not parts[0].strip().isdigit():
                continue
            wid = int(parts[0].strip())
            wname = parts[1].strip()
            if spool_ready(wname) or spool_gave_up(wname):
                continue
            jid = wname.removeprefix("usnm_quality_")
            req_files = [f"{jid}_page_jpg", f"{jid}_loc_xml", f"{jid}_texts_json", f"{jid}_quality_json"]
            if not all(os.path.exists(dir_hier_path(f)) and os.path.getsize(dir_hier_path(f)) > 0 for f in req_files):
                run_mysql(f"""
                    UPDATE result SET server_state = 5, outcome = 5 WHERE workunitid = {wid} AND server_state = 2;
                    UPDATE workunit SET name = CONCAT(name, '_old_', id), assimilate_state = 2, error_mask = 16, transition_time = 2147483647 WHERE id = {wid};
                """)
                cancelled_missing_dl += 1
    except Exception as e:
        print(f"missing dl cleanup warning: {e}", file=sys.stderr, flush=True)

    gpu_info = ungate_and_fill_gpu_queues()
    try:
        Path(f"{PROJ}/reread_db").touch()
    except Exception:
        pass
    os.makedirs(UNITS_DIR, exist_ok=True)
    os.makedirs(COLLECTED_DIR, exist_ok=True)
    mysql_wus = query_mysql_workunits()

    removed_ghosts = []
    resubmitted_orphans = 0
    recovered_terminal = 0
    known_jids = {}
    for bdir in sorted(glob.glob(f"{UNITS_DIR}/bundle-*")):
        udir = Path(bdir)
        qfile = udir / "quality.json"
        if not qfile.is_file() or qfile.stat().st_size == 0:
            shutil.rmtree(udir, ignore_errors=True)
            continue
        try:
            job = json.loads(qfile.read_text())
            jid = job["id"]
            wu = f"usnm_quality_{jid}"
            if (udir / "collected").exists():
                col_txt = (udir / "collected").read_text().strip()
                if col_txt.startswith("mysql_terminal_2_0") and spool_ready(wu):
                    (udir / "collected").unlink(missing_ok=True)
                    recovered_terminal += 1
                else:
                    known_jids[jid] = udir
                    continue

            phys_names = [
                f"{jid}_page_jpg",
                f"{jid}_loc_xml",
                f"{jid}_texts_json",
                f"usnm_quality_places_{job['inputs']['places']['sha256']}",
                f"{jid}_quality_json",
            ]
            has_all_downloads = all(
                os.path.exists(dir_hier_path(p)) and os.path.getsize(dir_hier_path(p)) > 0
                for p in phys_names
            )
            in_mysql = mysql_wus and wu in mysql_wus
            active_in_mysql = in_mysql and mysql_wus[wu][2] > 0 and mysql_wus[wu][1] == 0

            if not (udir / "submitted").exists():
                if active_in_mysql or spool_ready(wu):
                    (udir / "submitted").write_text(wu + "\n")
                else:
                    rb, rf = purge_unit_downloads(jid)
                    reclaimed_bytes += rb
                    reclaimed_files += rf
                    removed_ghosts.append(udir.name)
                    shutil.rmtree(udir, ignore_errors=True)
                    continue
            elif not active_in_mysql and not spool_ready(wu) and not spool_gave_up(wu):
                if in_mysql:
                    rename_colliding_workunit(wu)
                if has_all_downloads:
                    n_pages = max(1, len(job.get("pages") or []))
                    total_input = (
                        int(job["inputs"]["page"]["bytes"])
                        + int(job["inputs"]["alto"]["bytes"])
                        + int(job["inputs"]["texts"]["bytes"])
                        + int(job["inputs"]["places"]["bytes"])
                    )
                    disk_bound = max(2 * 1024 * 1024 * 1024, total_input + 512 * 1024 * 1024)
                    cw_args = [
                        f"{PROJ}/bin/create_work",
                        "--appname", "usnm_quality",
                        "--wu_name", wu,
                        "--wu_template", "templates/usnm_quality_in",
                        "--result_template", "templates/usnm_quality_out",
                        "--min_quorum", "2",
                        "--target_nresults", "2",
                        "--max_error_results", "3",
                        "--max_total_results", "4",
                        "--max_success_results", "3",
                        "--rsc_fpops_est", f"{n_pages}e12",
                        "--rsc_fpops_bound", f"{n_pages}e14",
                        "--rsc_memory_bound", "1610612736",
                        "--rsc_disk_bound", str(disk_bound),
                        "--delay_bound", "259200",
                        *phys_names,
                    ]
                    subprocess.run(cw_args, cwd=PROJ, check=True)
                    (udir / "submitted").write_text(wu + "\n")
                    resubmitted_orphans += 1
                else:
                    rb, rf = purge_unit_downloads(jid)
                    reclaimed_bytes += rb
                    reclaimed_files += rf
                    removed_ghosts.append(udir.name)
                    shutil.rmtree(udir, ignore_errors=True)
                    continue
            known_jids[jid] = udir
            for fn in ("page.jpg", "loc.xml", "places.txt"):
                fp = udir / fn
                if not fp.exists() or fp.stat().st_size > 0:
                    open(fp, "wb").close()
            tfile = udir / "texts.json"
            expected_t = int(job["inputs"]["texts"]["bytes"])
            if not tfile.exists() or tfile.stat().st_size != expected_t:
                dl_t = dir_hier_path(f"{jid}_texts_json")
                if os.path.exists(dl_t) and os.path.getsize(dl_t) > 0:
                    shutil.copyfile(dl_t, tfile)
                elif not tfile.exists():
                    open(tfile, "wb").close()
        except Exception:
            pass

    diag = {}
    try:
        tr_logs = sorted(glob.glob(f"{PROJ}/log_*/transitioner.log"))
        if tr_logs:
            diag["transitioner_log_tail"] = Path(tr_logs[-1]).read_text(errors="replace").splitlines()[-35:]
        tr_run = subprocess.run(
            ["su", "-s", "/bin/bash", "boincadm", "-c", f"cd '{PROJ}' && ./bin/transitioner -d 3 --one_pass"],
            capture_output=True, text=True, timeout=60
        )
        diag["transitioner_one_pass_rc"] = tr_run.returncode
        diag["transitioner_one_pass_out"] = (tr_run.stdout + "\n" + tr_run.stderr).splitlines()[-30:]
        try:
            Path(f"{PROJ}/reread_db").touch()
            time.sleep(2)
        except Exception:
            pass
        if tr_logs:
            diag["transitioner_log_after"] = Path(tr_logs[-1]).read_text(errors="replace").splitlines()[-25:]
        fd_logs = sorted(glob.glob(f"{PROJ}/log_*/feeder.log"), key=lambda p: os.path.getmtime(p))
        if fd_logs:
            diag["feeder_tail"] = Path(fd_logs[-1]).read_text(errors="replace").splitlines()[-35:]
        sc_logs = sorted(glob.glob(f"{PROJ}/log_*/scheduler.log"), key=lambda p: os.path.getmtime(p))
        if sc_logs:
            sc_lines = Path(sc_logs[-1]).read_text(errors="replace").splitlines()
            diag["scheduler_log_file"] = sc_logs[-1]
            diag["scheduler_host5"] = [l for l in sc_lines if "HOST#5" in l or "host 5" in l or "hostid=5" in l][-40:]
            diag["scheduler_tail"] = sc_lines[-40:]
        diag["host5_sql"] = run_mysql("SELECT * FROM host WHERE id = 5\\G").strip().splitlines()
        diag["host5_results_sql"] = run_mysql("""
            SELECT r.id, r.name, r.server_state, r.outcome, r.exit_status, a.name
            FROM result r JOIN app a ON r.appid = a.id
            WHERE r.hostid = 5 ORDER BY r.id DESC LIMIT 15;
        """).strip().splitlines()
        diag["cancelled_missing_dl_wus"] = cancelled_missing_dl
        diag["gpu_info"] = gpu_info
        diag["qwen_queue_sql"] = run_mysql("""
            SELECT w.batch, r.server_state, COUNT(*)
            FROM result r JOIN workunit w ON r.workunitid = w.id JOIN app a ON w.appid = a.id
            WHERE a.name = 'usnm_qwen'
            GROUP BY w.batch, r.server_state;
        """).strip().splitlines()
        diag["quality_queue_sql"] = run_mysql("""
            SELECT r.server_state, COUNT(*)
            FROM result r JOIN app a ON r.appid = a.id
            WHERE a.name = 'usnm_quality'
            GROUP BY r.server_state;
        """).strip().splitlines()
    except Exception as e:
        diag["diag_error"] = str(e)

    return {
        "reclaimed_mb": round(reclaimed_bytes / (1024 * 1024), 1),
        "reclaimed_files": reclaimed_files,
        "removed_ghost_bundles": len(removed_ghosts),
        "resubmitted_orphan_bundles": resubmitted_orphans,
        "recovered_terminal_bundles": recovered_terminal,
        "known_bundles": len(known_jids),
        "diag": diag,
    }


def collect_and_purge(force=False):
    """
    Collects all newly assimilated bundles into Parquet chunks using
    `usnm-boinc quality collect` (v1.3 63-column schema), reconciles against MySQL
    workunit state, purges download/spool/texts.json files from /project, and
    uploads the merged `pages.parquet` + `collector.json` status to Azure Blob Storage.
    """
    os.makedirs(LOCAL_LOCKS_DIR, exist_ok=True)
    os.makedirs(LOCKS_DIR, exist_ok=True)
    os.makedirs(COLLECTED_DIR, exist_ok=True)
    os.makedirs(STAGED_META_DIR, exist_ok=True)
    ts_marker = f"{LOCAL_LOCKS_DIR}/last_collect.ts"
    if not force and os.path.exists(ts_marker):
        try:
            if time.time() - float(Path(ts_marker).read_text().strip()) < 60.0:
                return None
        except Exception:
            pass

    lock_f = open(f"{LOCAL_LOCKS_DIR}/collect.lock", "w")
    try:
        fcntl.flock(lock_f, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        lock_f.close()
        return None

    try:
        Path(ts_marker).write_text(str(time.time()))
        try:
            if os.path.exists(SPOOL_DIR):
                os.listdir(SPOOL_DIR)
        except Exception:
            pass
        rep_marker = f"{LOCAL_LOCKS_DIR}/repaired_v10.done"
        if not os.path.exists(rep_marker):
            repair_info = repair_and_reclaim_download_space()
            Path(rep_marker).write_text(json.dumps(repair_info))
        else:
            try:
                repair_info = json.loads(Path(rep_marker).read_text())
            except Exception:
                repair_info = {}

        mysql_wus = query_mysql_workunits()
        ready_units = []
        pending_units = 0
        already_collected_units = 0

        for bdir in sorted(glob.glob(f"{UNITS_DIR}/bundle-*")):
            udir = Path(bdir)
            if (udir / "collected").exists():
                already_collected_units += 1
                continue
            if not (udir / "submitted").exists():
                continue
            wu = (udir / "submitted").read_text().strip()
            jid = wu.removeprefix("usnm_quality_")
            if spool_ready(wu):
                ready_units.append((udir, wu))
            elif spool_gave_up(wu):
                (udir / "collected").write_text("gave_up\n")
                purge_unit_downloads(jid)
                already_collected_units += 1
            elif mysql_wus and wu not in mysql_wus:
                # Ghost unit that was never created in BOINC MySQL; remove so pages get re-staged
                purge_unit_downloads(jid)
                shutil.rmtree(udir, ignore_errors=True)
            elif wu in mysql_wus and (
                mysql_wus[wu][1] != 0
                or (mysql_wus[wu][0] == 2 and (time.time() - udir.stat().st_mtime > 7200))
            ):
                # Workunit errored or assimilated >2h ago in MySQL without spool files
                assim_st, err_m, _ = mysql_wus[wu]
                (udir / "collected").write_text(f"mysql_terminal_{assim_st}_{err_m}\n")
                purge_unit_downloads(jid)
                already_collected_units += 1
            else:
                pending_units += 1

        put_status("collector.json", {
            "ts": time.time(),
            "phase": "collecting_chunks",
            "ready_units": len(ready_units),
            "already_collected_bundles": already_collected_units,
            "pending_uncollected_bundles": pending_units,
            "repair_info": repair_info,
        })

        newly_collected = 0
        for chunk_idx in range(0, len(ready_units), 20):
            chunk = ready_units[chunk_idx:chunk_idx + 20]
            tmp_root = Path(f"/tmp/w9_collect_{os.getpid()}_{chunk_idx}")
            tmp_units = tmp_root / "units"
            tmp_out = tmp_root / "out"
            tmp_qwen = tmp_root / "qwen"
            tmp_qwen_spool = tmp_root / "qwen_spool"
            shutil.rmtree(tmp_root, ignore_errors=True)
            tmp_units.mkdir(parents=True, exist_ok=True)
            tmp_out.mkdir(parents=True, exist_ok=True)
            tmp_qwen.mkdir(parents=True, exist_ok=True)
            tmp_qwen_spool.mkdir(parents=True, exist_ok=True)

            for udir, _ in chunk:
                os.symlink(str(udir), str(tmp_units / udir.name))

            cmd = [
                "/usr/local/bin/usnm-boinc", "quality", "collect",
                "--campaign", "v1-wave9",
                "--root", str(tmp_root),
                "--spool", SPOOL_DIR,
                "--qwen-root", str(tmp_qwen),
                "--qwen-spool", str(tmp_qwen_spool),
                "--out-dir", str(tmp_out),
            ]
            subprocess.run(cmd, check=True)

            part_pq = tmp_out / "pages.parquet"
            if part_pq.exists() and part_pq.stat().st_size > 0:
                part_name = f"part-{int(time.time())}-{chunk_idx:04d}-{len(chunk):04d}.parquet"
                dst_part = Path(COLLECTED_DIR) / part_name
                shutil.copyfile(part_pq, dst_part)
                put_blob(
                    f"audit/quality/v1-wave9/parts/{part_name}",
                    dst_part.read_bytes(),
                    "application/vnd.apache.parquet",
                )

            for udir, wu in chunk:
                jid = wu.removeprefix("usnm_quality_")
                (udir / "collected").write_text(f"{int(time.time())}\n")
                tfile = udir / "texts.json"
                if tfile.exists():
                    open(tfile, "wb").close()
                purge_unit_downloads(jid)
                for fn in ("result.json", "confusions.json", "diag.txt"):
                    sp = f"{SPOOL_DIR}/{wu}__file_{fn}"
                    if os.path.exists(sp):
                        try:
                            os.remove(sp)
                        except OSError:
                            pass
                newly_collected += 1
                already_collected_units += 1

            shutil.rmtree(tmp_root, ignore_errors=True)
            put_status("collector.json", {
                "ts": time.time(),
                "phase": f"collected_chunk_{chunk_idx}",
                "newly_collected_bundles": newly_collected,
                "total_collected_bundles": already_collected_units,
                "pending_uncollected_bundles": pending_units,
                "repair_info": repair_info,
            })

        part_files = sorted(glob.glob(f"{COLLECTED_DIR}/part-*.parquet"))
        stats_cache_file = Path(f"{LOCKS_DIR}/last_merged_stats.json")
        merged_stats = {}
        if stats_cache_file.is_file():
            try:
                merged_stats = json.loads(stats_cache_file.read_text())
            except Exception:
                merged_stats = {}
        if part_files and (newly_collected > 0 or not stats_cache_file.is_file()):
            import duckdb
            con = duckdb.connect()
            con.execute("SET memory_limit='1GB'; SET threads=2;")
            merged_pq = f"/tmp/w9_merged_{os.getpid()}.parquet"

            existing_cols = {
                r[0]
                for r in con.execute(
                    f"DESCRIBE SELECT * FROM read_parquet({part_files}, union_by_name=true)"
                ).fetchall()
            }

            replace_exprs = []
            extra_exprs = []
            for c in DOUBLE_COLS:
                if c in existing_cols:
                    replace_exprs.append(f"TRY_CAST(p.{c} AS DOUBLE) AS {c}")
                else:
                    extra_exprs.append(f"CAST(NULL AS DOUBLE) AS {c}")
            for c in BIGINT_COLS:
                if c in existing_cols:
                    replace_exprs.append(f"TRY_CAST(p.{c} AS BIGINT) AS {c}")
                else:
                    extra_exprs.append(f"CAST(NULL AS BIGINT) AS {c}")
            for c in BOOL_COLS:
                if c in existing_cols:
                    replace_exprs.append(f"TRY_CAST(p.{c} AS BOOLEAN) AS {c}")
                else:
                    extra_exprs.append(f"CAST(NULL AS BOOLEAN) AS {c}")

            replace_sql = f"REPLACE ({', '.join(replace_exprs)})" if replace_exprs else ""
            extra_sql = (", " + ", ".join(extra_exprs)) if extra_exprs else ""

            meta_files = sorted(glob.glob(f"{STAGED_META_DIR}/*.parquet"))
            if meta_files:
                meta_union_sql = f"""
                    SELECT doc_id, ANY_VALUE(batch) AS batch, ANY_VALUE(batch_version) AS batch_version, ANY_VALUE(date) AS date
                    FROM (
                        SELECT doc_id, batch, CAST(batch_version AS BIGINT) AS batch_version, CAST(date AS VARCHAR) AS date
                        FROM read_parquet('{SHARED_MANIFEST}')
                        UNION ALL
                        SELECT doc_id, batch, CAST(batch_version AS BIGINT) AS batch_version, CAST(date AS VARCHAR) AS date
                        FROM read_parquet({meta_files}, union_by_name=true)
                    )
                    GROUP BY doc_id
                """
            else:
                meta_union_sql = f"""
                    SELECT doc_id, ANY_VALUE(batch) AS batch, ANY_VALUE(batch_version) AS batch_version, ANY_VALUE(date) AS date
                    FROM read_parquet('{SHARED_MANIFEST}')
                    GROUP BY doc_id
                """

            con.execute(f"""
                COPY (
                    SELECT
                        COALESCE(m.batch, 'unknown') AS batch,
                        COALESCE(CAST(m.batch_version AS BIGINT), 1) AS batch_version,
                        COALESCE(
                            TRY_CAST(SUBSTR(CAST(m.date AS VARCHAR), 1, 4) AS BIGINT),
                            TRY_CAST(REGEXP_EXTRACT(p.doc_id, '_([0-9]{{4}})-', 1) AS BIGINT)
                        ) AS year,
                        (COALESCE(
                            TRY_CAST(SUBSTR(CAST(m.date AS VARCHAR), 1, 4) AS BIGINT),
                            TRY_CAST(REGEXP_EXTRACT(p.doc_id, '_([0-9]{{4}})-', 1) AS BIGINT)
                        ) // 10) * 10 AS decade,
                        p.* {replace_sql}
                        {extra_sql}
                    FROM read_parquet({part_files}, union_by_name=true) p
                    LEFT JOIN ({meta_union_sql}) m USING (doc_id)
                    QUALIFY ROW_NUMBER() OVER (PARTITION BY p.doc_id, p.reader ORDER BY p.job_id DESC) = 1
                    ORDER BY batch, p.doc_id, p.reader
                ) TO '{merged_pq}' (FORMAT PARQUET, COMPRESSION ZSTD)
            """)
            merged_bytes = Path(merged_pq).read_bytes()
            put_blob("audit/quality/v1-wave9/pages.parquet", merged_bytes, "application/vnd.apache.parquet")

            stats_rows = con.execute(f"""
                SELECT
                    reader,
                    COUNT(*) AS rows,
                    COUNT(DISTINCT doc_id) AS pages,
                    COUNT(DISTINCT batch) AS batches,
                    ROUND(AVG(TRY_CAST(est_cer AS DOUBLE)), 4) AS mean_est_cer,
                    ROUND(AVG(completeness), 4) AS mean_completeness,
                    ROUND(AVG(precision), 4) AS mean_precision,
                    ROUND(AVG(damage_rate), 4) AS mean_damage_rate,
                    ROUND(AVG(function_share), 4) AS mean_function_share,
                    ROUND(AVG(garbage_share), 4) AS mean_garbage_share,
                    ROUND(AVG(blur), 2) AS mean_blur,
                    ROUND(AVG(contrast), 2) AS mean_contrast,
                    ROUND(AVG(otsu_local_diff), 4) AS mean_otsu_local_diff,
                    ROUND(AVG(bg_tile_std), 2) AS mean_bg_tile_std,
                    ROUND(AVG(bleed_through), 4) AS mean_bleed_through,
                    ROUND(AVG(est_line_height_px), 2) AS mean_est_line_height_px,
                    ROUND(AVG(vertical_stroke_ratio), 4) AS mean_vertical_stroke_ratio,
                    ROUND(AVG(fraktur_score), 4) AS mean_fraktur_score,
                    ROUND(AVG(skew_deg), 3) AS mean_skew_deg,
                    ROUND(AVG(ink_density), 4) AS mean_ink_density,
                    ROUND(AVG(total_ms), 1) AS mean_total_ms
                FROM read_parquet('{merged_pq}')
                GROUP BY reader
                ORDER BY reader
            """).fetchall()
            cols = [
                "reader", "rows", "pages", "batches", "mean_est_cer", "mean_completeness",
                "mean_precision", "mean_damage_rate", "mean_function_share",
                "mean_garbage_share", "mean_blur", "mean_contrast",
                "mean_otsu_local_diff", "mean_bg_tile_std", "mean_bleed_through",
                "mean_est_line_height_px", "mean_vertical_stroke_ratio", "mean_fraktur_score",
                "mean_skew_deg", "mean_ink_density", "mean_total_ms",
            ]
            merged_stats["by_reader"] = [dict(zip(cols, r)) for r in stats_rows]
            totals = con.execute(f"""
                SELECT
                    COUNT(*) AS total_rows,
                    COUNT(DISTINCT doc_id) AS distinct_pages,
                    COUNT(DISTINCT batch) AS distinct_batches,
                    COUNT(DISTINCT job_id) AS distinct_bundles,
                    COUNT(CASE WHEN damage_rate IS NOT NULL THEN 1 END) AS rows_with_damage_rate,
                    COUNT(CASE WHEN blur IS NOT NULL THEN 1 END) AS rows_with_image_stats,
                    COUNT(CASE WHEN otsu_local_diff IS NOT NULL THEN 1 END) AS rows_with_v13_metrics,
                    COUNT(DISTINCT CASE WHEN otsu_local_diff IS NOT NULL THEN doc_id END) AS pages_with_v13_metrics
                FROM read_parquet('{merged_pq}')
            """).fetchone()
            merged_stats["totals"] = {
                "total_rows": totals[0],
                "distinct_pages": totals[1],
                "distinct_batches": totals[2],
                "distinct_bundles": totals[3],
                "rows_with_damage_rate": totals[4],
                "rows_with_image_stats": totals[5],
                "rows_with_v13_metrics": totals[6],
                "pages_with_v13_metrics": totals[7],
                "parquet_bytes": len(merged_bytes),
            }
            try:
                stats_cache_file.write_text(json.dumps(merged_stats))
            except Exception:
                pass
            os.remove(merged_pq)

        du = shutil.disk_usage("/project")
        wu_sql = run_mysql("""
            SELECT
                COUNT(*) AS total_wus,
                SUM(CASE WHEN assimilate_state=2 THEN 1 ELSE 0 END) AS assimilated_wus,
                SUM(CASE WHEN assimilate_state=1 THEN 1 ELSE 0 END) AS ready_to_assimilate,
                SUM(CASE WHEN assimilate_state=0 AND error_mask=0 THEN 1 ELSE 0 END) AS in_progress_wus,
                SUM(CASE WHEN error_mask!=0 THEN 1 ELSE 0 END) AS error_wus
            FROM workunit
            WHERE name LIKE 'usnm_quality_%';
        """).strip().splitlines()

        c_status = {
            "ts": time.time(),
            "newly_collected_bundles": newly_collected,
            "total_collected_bundles": already_collected_units,
            "pending_uncollected_bundles": pending_units,
            "repair_info": repair_info,
            "project_disk": {
                "total_gb": round(du.total / (1024**3), 2),
                "used_gb": round(du.used / (1024**3), 2),
                "free_gb": round(du.free / (1024**3), 2),
            },
            "mysql_workunits": wu_sql[-1].split("\t") if len(wu_sql) >= 2 else wu_sql,
            "merged_stats": merged_stats,
        }
        put_status("collector.json", c_status)
        return c_status
    finally:
        fcntl.flock(lock_f, fcntl.LOCK_UN)
        lock_f.close()


def count_in_flight_bundles():
    n = 0
    for bdir in glob.glob(f"{UNITS_DIR}/bundle-*"):
        udir = Path(bdir)
        if (udir / "submitted").exists() and not (udir / "collected").exists():
            n += 1
    return n


def wait_for_queue_headroom(shard_idx, status):
    while True:
        touch_shard_heartbeat(shard_idx)
        in_flight = count_in_flight_bundles()
        free_gb = shutil.disk_usage("/project").free / (1024**3)
        if in_flight < MAX_IN_FLIGHT_BUNDLES and free_gb >= MIN_FREE_GB:
            status["state"] = "running"
            status["in_flight_bundles"] = in_flight
            status["project_free_gb"] = round(free_gb, 2)
            return
        collect_and_purge(force=True)
        in_flight = count_in_flight_bundles()
        free_gb = shutil.disk_usage("/project").free / (1024**3)
        if in_flight < MAX_IN_FLIGHT_BUNDLES and free_gb >= MIN_FREE_GB:
            status["state"] = "running"
            status["in_flight_bundles"] = in_flight
            status["project_free_gb"] = round(free_gb, 2)
            return
        status["state"] = "waiting_for_boinc_drain"
        status["in_flight_bundles"] = in_flight
        status["project_free_gb"] = round(free_gb, 2)
        put_status(f"shard-{shard_idx}.json", status)
        time.sleep(30)


def init_wave9():
    os.makedirs(LOCAL_LOCKS_DIR, exist_ok=True)
    os.makedirs(UNITS_DIR, exist_ok=True)
    os.makedirs(COLLECTED_DIR, exist_ok=True)
    os.makedirs(STAGED_META_DIR, exist_ok=True)
    os.makedirs(BATCH_INDEX_DIR, exist_ok=True)
    os.makedirs(LOCKS_DIR, exist_ok=True)
    os.makedirs(FW_DIR, exist_ok=True)
    init_lock = open(f"{LOCAL_LOCKS_DIR}/init.lock", "w")
    fcntl.flock(init_lock, fcntl.LOCK_EX)
    try:
        init_done_local = f"{LOCAL_LOCKS_DIR}/init.done"
        if os.path.exists(init_done_local):
            return

        if not os.path.exists(PLACES_PATH):
            candidates = (
                glob.glob("/project/quality/*/places.txt")
                + glob.glob("/project/quality/*/texts/places.txt")
            )
            if candidates:
                shutil.copyfile(candidates[0], PLACES_PATH)
            else:
                download_blob(f"{HANDOFF_BASE}/quality/v1-2k/places.txt?{HANDOFF_SAS}", PLACES_PATH)

        for lang in ALL_LANGS:
            fw_dst = f"{FW_DIR}/{lang}.txt"
            if not os.path.exists(fw_dst):
                subprocess.check_call([
                    "/usr/local/bin/usnm-boinc", "quality", "function-words",
                    "--lang", lang,
                    "--out-dir", FW_DIR,
                ])
        for alias, target in LANG_ALIASES.items():
            alias_dst = f"{FW_DIR}/{alias}.txt"
            target_src = f"{FW_DIR}/{target}.txt"
            if not os.path.exists(alias_dst) and os.path.exists(target_src):
                shutil.copyfile(target_src, alias_dst)

        if not os.path.exists(SHARED_MANIFEST):
            download_blob(
                f"{HANDOFF_BASE}/quality/v1-wave9/wave9_manifest.parquet?{HANDOFF_SAS}",
                SHARED_MANIFEST,
            )

        # Deprecate any usnm_quality version < 103 (v1.3) and clear host daily quotas across all platforms
        run_mysql("""
            UPDATE app_version SET deprecated = CASE WHEN version_num < 103 THEN 1 ELSE 0 END
            WHERE appid = (SELECT id FROM app WHERE name = 'usnm_quality');
            UPDATE host_app_version SET max_jobs_per_day = 50000, n_jobs_today = 0;
            UPDATE host SET nresults_today = 0;
        """)
        shutil.rmtree(f"{LOCKS_DIR}/collect.lock", ignore_errors=True)
        for stale_p in glob.glob(f"{LOCKS_DIR}/shard-*.lock") + glob.glob(f"{LOCKS_DIR}/shard-*.done"):
            shutil.rmtree(stale_p, ignore_errors=True)
            try:
                os.remove(stale_p)
            except OSError:
                pass
        try:
            os.remove(f"{LOCAL_LOCKS_DIR}/repaired_v4.done")
        except OSError:
            pass
        collect_and_purge(force=True)
        Path(init_done_local).write_text("ok\n")
    finally:
        fcntl.flock(init_lock, fcntl.LOCK_UN)
        init_lock.close()


def write_empty_alto(dst, width, height):
    xml = (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<alto xmlns="http://www.loc.gov/standards/alto/ns-v3#">'
        "<Description><MeasurementUnit>pixel</MeasurementUnit></Description>"
        f'<Layout><Page ID="P1" WIDTH="{width}" HEIGHT="{height}" PHYSICAL_IMG_NR="1">'
        f'<PrintSpace HPOS="0" VPOS="0" WIDTH="{width}" HEIGHT="{height}"/>'
        "</Page></Layout></alto>\n"
    )
    with open(dst, "w", encoding="utf-8") as f:
        f.write(xml)


def normalize_text(txt):
    if isinstance(txt, (list, tuple)):
        txt = txt[0] if (txt and isinstance(txt[0], str)) else "\n".join(str(x) for x in txt)
    if not isinstance(txt, str):
        txt = str(txt or "")
    txt = "".join(ch for ch in txt if ch in ("\n", "\r", "\t") or ord(ch) >= 32)
    b = txt.encode("utf-8")
    if len(b) > 500_000:
        txt = b[:500_000].decode("utf-8", errors="ignore")
    return txt


def to_sample_row(r):
    dr = float(r.get("damage_rate") or 0.15)
    dmg = "low" if dr < 0.1 else ("mid" if dr < 0.5 else "high")
    date_str = str(r.get("date") or r["doc_id"].split("_")[1])
    yr = int(date_str[:4]) if date_str[:4].isdigit() else 1900
    batch = str(r.get("batch") or "unk_batch")
    raw_lang = str(r.get("lang") or "eng")
    lang = LANG_ALIASES.get(raw_lang, raw_lang)
    if lang not in ALL_LANGS:
        lang = "eng"
    return {
        "batch": batch,
        "batch_version": int(r.get("batch_version") or 1),
        "crash_weight": 1.0,
        "damage": dmg,
        "damage_rate": dr,
        "decade": (yr // 10) * 10,
        "doc_id": r["doc_id"],
        "lang": lang,
        "mode": "ocr",
        "prefix": batch.split("_")[0],
        "stratum_pages": 1,
        "weight": 1.0,
        "year": yr,
    }


def stage_stream(project: str, physical: str, src_path: Path):
    dst = Path(dir_hier_path(physical))
    dst.parent.mkdir(parents=True, exist_ok=True)
    if not dst.is_file() or dst.stat().st_size != src_path.stat().st_size:
        tmp = dst.with_suffix(f".tmp.{os.getpid()}")
        shutil.copyfile(src_path, tmp)
        os.chmod(tmp, 0o644)
        os.replace(tmp, dst)


def load_all_existing_bundled_docs(shard_idx):
    bundled_docs = set()
    skipped_path = Path(f"{WAVE9_ROOT}/skipped_docs.txt")
    if skipped_path.is_file():
        try:
            for line in skipped_path.read_text().splitlines():
                s = line.strip()
                if s:
                    bundled_docs.add(s)
        except Exception:
            pass
    max_seq = 0
    for qpath in glob.glob(f"{UNITS_DIR}/bundle-*/quality.json"):
        udir = Path(os.path.dirname(qpath))
        bname = udir.name
        parts = bname.split("-")
        if len(parts) >= 3 and parts[1] == f"s{shard_idx}" and parts[2].isdigit():
            max_seq = max(max_seq, int(parts[2]))
        if not (udir / "submitted").exists() and not (udir / "collected").exists():
            continue
        try:
            job = json.loads(Path(qpath).read_text())
            for p in job.get("pages") or []:
                if p.get("doc_id"):
                    bundled_docs.add(p["doc_id"])
            if not job.get("pages") and job.get("doc_id"):
                bundled_docs.add(job["doc_id"])
        except Exception:
            pass
    return bundled_docs, max_seq


def record_skipped_docs(doc_ids):
    if not doc_ids:
        return
    try:
        lock_f = open(f"{LOCAL_LOCKS_DIR}/skipped_docs.lock", "w")
        fcntl.flock(lock_f, fcntl.LOCK_EX)
        try:
            with open(f"{WAVE9_ROOT}/skipped_docs.txt", "a", encoding="utf-8") as f:
                for did in doc_ids:
                    f.write(f"{did}\n")
        finally:
            fcntl.flock(lock_f, fcntl.LOCK_UN)
            lock_f.close()
    except Exception:
        pass


def parse_mets_pages(xml_bytes):
    """
    Python port of crates/usnm-ocr-worker/src/batch.rs::mets_pages:
    Maps seq (int) -> frame stem (str, e.g. '0001') from an issue METS XML.
    """
    root = ET.fromstring(xml_bytes)
    starts = {}
    files = {}
    pages = []

    def local(tag):
        return tag.split("}", 1)[-1] if "}" in tag else tag

    for elem in root.iter():
        ln = local(elem.tag)
        if ln == "dmdSec":
            dmd_id = elem.attrib.get("ID")
            if dmd_id:
                for sub in elem.iter():
                    if local(sub.tag) == "start" and sub.text and sub.text.strip():
                        starts.setdefault(dmd_id, sub.text.strip())
        elif ln == "file":
            fid = elem.attrib.get("ID")
            usage = elem.attrib.get("USE", "")
            if fid:
                for sub in elem.iter():
                    if local(sub.tag) == "FLocat":
                        href = None
                        for k, v in sub.attrib.items():
                            if local(k) == "href":
                                href = v
                                break
                        if href:
                            files.setdefault(fid, (usage, href))
        elif ln == "div" and elem.attrib.get("TYPE") == "np:page":
            dmd_id = elem.attrib.get("DMDID", "")
            fids = [
                sub.attrib["FILEID"]
                for sub in elem.iter()
                if local(sub.tag) == "fptr" and "FILEID" in sub.attrib
            ]
            pages.append((dmd_id, fids))

    out = {}
    for dmd_id, fids in pages:
        s_txt = starts.get(dmd_id, "")
        if not s_txt.isdigit():
            continue
        seq = int(s_txt)
        frame = None
        for want_service in (True, False):
            for fid in fids:
                if fid not in files:
                    continue
                usage, href = files[fid]
                if want_service and usage != "service":
                    continue
                stem = href.removeprefix("./").split("/")[-1]
                if stem.endswith(".jp2"):
                    frame = stem[:-4]
                    break
            if frame:
                break
        if frame:
            out[seq] = frame
    return out


class LocBatchResolver:
    """
    Resolves (lccn, date, edition, seq) -> (jp2_url, alto_url) on tile.loc.gov
    using the batch's manifest-md5.txt and issue METS XML, cached in BATCH_INDEX_DIR.
    """
    def __init__(self, batch, bv):
        self.batch = batch
        self.bv = int(bv)
        self.batch_dir = f"{batch}_ver{self.bv:02d}"
        inst = batch.split("_")[0]
        self.batch_url = f"{LOC_STORAGE_ROOT}{inst}/batch_{self.batch_dir}/"
        self.cache_file = Path(BATCH_INDEX_DIR) / f"{self.batch_dir}.json"
        self.loaded = False
        self.issues = {}  # "lccn/issue_dir" -> {"reel": str, "mets": str, "frames": {str(seq): frame}}

    def _ensure_manifest(self):
        if self.loaded:
            return
        self.loaded = True
        if self.cache_file.exists():
            try:
                self.issues = json.loads(self.cache_file.read_text())
                return
            except Exception:
                pass
        raw = loc_paced_download(f"{self.batch_url}manifest-md5.txt").decode("utf-8", errors="ignore")
        issues = {}
        for line in raw.splitlines():
            parts = line.strip().split(None, 1)
            if len(parts) != 2:
                continue
            path = parts[1].strip().lstrip("*")
            segs = path.split("/")
            if len(segs) != 5 or segs[0] != "data":
                continue
            _, lccn, reel, idir, fname = segs
            key = f"{lccn}/{idir}"
            ent = issues.setdefault(key, {"reel": reel, "mets": None, "frames": {}})
            if ent["reel"] != reel:
                continue
            if fname == f"{idir}.xml":
                ent["mets"] = fname
            elif fname == f"{idir}_1.xml" and not ent["mets"]:
                ent["mets"] = fname
        self.issues = issues
        self._save_cache()

    def _save_cache(self):
        try:
            tmp = f"{self.cache_file}.tmp.{os.getpid()}"
            Path(tmp).write_text(json.dumps(self.issues))
            os.replace(tmp, self.cache_file)
        except Exception:
            pass

    def resolve(self, lccn, date_str, edition, seq):
        self._ensure_manifest()
        ymd = str(date_str).replace("-", "")[:8]
        idir = f"{ymd}{int(edition):02d}"
        key = f"{lccn}/{idir}"
        ent = self.issues.get(key)
        if not ent or not ent.get("mets"):
            return None, None
        frames = ent.get("frames") or {}
        if not frames:
            mets_url = f"{self.batch_url}data/{lccn}/{ent['reel']}/{idir}/{ent['mets']}"
            xml_bytes = loc_paced_download(mets_url)
            parsed = parse_mets_pages(xml_bytes)
            frames = {str(k): v for k, v in parsed.items()}
            ent["frames"] = frames
            self._save_cache()
        frame = frames.get(str(int(seq)))
        if not frame:
            return None, None
        base_path = f"{self.batch_url}data/{lccn}/{ent['reel']}/{idir}/{frame}"
        return f"{base_path}.jp2", f"{base_path}.xml"


def flush_bundle(shard_idx, chunk_rows, bundle_seq, stage_dir, scans_dir, texts_dir, status):
    if not chunk_rows:
        return 0
    wait_for_queue_headroom(shard_idx, status)

    rows_to_stage = list(chunk_rows)
    sample_jsonl = f"{stage_dir}/sample.jsonl"
    grp = f"wave9-s{shard_idx}"

    while rows_to_stage:
        with open(sample_jsonl, "w") as f:
            for _, r in rows_to_stage:
                f.write(json.dumps(to_sample_row(r)) + "\n")
        cmd = [
            "/usr/local/bin/usnm-boinc", "quality", "stage",
            "--root", stage_dir,
            "--scans-dir", scans_dir,
            "--texts-dir", texts_dir,
            "--places", PLACES_PATH,
            "--function-words-dir", FW_DIR,
            "--engine-image", ENGINE_IMAGE,
            "--readers", READERS,
            "--sample", sample_jsonl,
            "--group", grp,
            "--max-pages", str(len(rows_to_stage) + 10),
            "--bundle-size", str(BUNDLE_SIZE),
        ]
        res = subprocess.run(cmd, capture_output=True, text=True)
        if res.returncode == 0:
            break
        hold_file = f"{stage_dir}/HOLD"
        if os.path.exists(hold_file):
            bad_line = open(hold_file).read().strip()
            bad_doc = bad_line.split(":")[0].strip()
            status["skipped_errors"].append(f"stage hold: {bad_line[:200]}")
            os.remove(hold_file)
            shutil.rmtree(f"{stage_dir}/pages/{bad_doc}", ignore_errors=True)
            before = len(rows_to_stage)
            rows_to_stage = [x for x in rows_to_stage if x[0] != bad_doc]
            if len(rows_to_stage) == before:
                break
        else:
            err_msg = f"stage error seq={bundle_seq}: {res.stderr[-300:]}"
            status["skipped_errors"].append(err_msg)
            put_status(f"shard-{shard_idx}.json", status)
            raise RuntimeError(err_msg)

    submitted_now = 0
    staged_bundles = sorted(glob.glob(f"{stage_dir}/units/bundle-*"))
    for bpath in staged_bundles:
        udir = Path(bpath)
        qfile = udir / "quality.json"
        if not qfile.is_file():
            continue
        job = json.loads(qfile.read_text())
        jid = job["id"]
        n_pages = max(1, len(job.get("pages") or []))
        wu = f"usnm_quality_{jid}"

        phys_map = [
            (f"{jid}_page_jpg", udir / "page.jpg"),
            (f"{jid}_loc_xml", udir / "loc.xml"),
            (f"{jid}_texts_json", udir / "texts.json"),
            (f"usnm_quality_places_{job['inputs']['places']['sha256']}", udir / "places.txt"),
            (f"{jid}_quality_json", qfile),
        ]
        for phys, src in phys_map:
            stage_stream(PROJ, phys, src)

        for fn in ("page.jpg", "loc.xml", "places.txt"):
            fp = udir / fn
            if fp.exists():
                open(fp, "wb").close()

        first_doc = job.get("doc_id", "bundle")
        dst_name = f"bundle-s{shard_idx}-{bundle_seq + submitted_now:04d}-{first_doc}"
        dst_path = Path(UNITS_DIR) / dst_name
        if dst_path.exists():
            shutil.rmtree(dst_path, ignore_errors=True)
        shutil.move(str(udir), str(dst_path))

        total_input = (
            int(job["inputs"]["page"]["bytes"])
            + int(job["inputs"]["alto"]["bytes"])
            + int(job["inputs"]["texts"]["bytes"])
            + int(job["inputs"]["places"]["bytes"])
        )
        disk_bound = max(2 * 1024 * 1024 * 1024, total_input + 512 * 1024 * 1024)
        cw_args = [
            f"{PROJ}/bin/create_work",
            "--appname", "usnm_quality",
            "--wu_name", wu,
            "--wu_template", "templates/usnm_quality_in",
            "--result_template", "templates/usnm_quality_out",
            "--min_quorum", "2",
            "--target_nresults", "2",
            "--max_error_results", "3",
            "--max_total_results", "4",
            "--max_success_results", "3",
            "--rsc_fpops_est", f"{n_pages}e12",
            "--rsc_fpops_bound", f"{n_pages}e14",
            "--rsc_memory_bound", "1610612736",
            "--rsc_disk_bound", str(disk_bound),
            "--delay_bound", "259200",
            *[p for p, _ in phys_map],
        ]
        lock_f = open(f"{LOCAL_LOCKS_DIR}/submit.lock", "w")
        fcntl.flock(lock_f, fcntl.LOCK_EX)
        try:
            res = subprocess.run(cw_args, cwd=PROJ, capture_output=True, text=True)
            if res.returncode != 0:
                rename_colliding_workunit(wu)
                subprocess.run(cw_args, cwd=PROJ, check=True)
            subprocess.run(
                ["su", "-s", "/bin/bash", "boincadm", "-c", f"cd '{PROJ}' && ./bin/transitioner -d 2 --one_pass"],
                capture_output=True, text=True, timeout=30
            )
        finally:
            fcntl.flock(lock_f, fcntl.LOCK_UN)
            lock_f.close()

        # Write submitted marker ONLY after create_work succeeds
        (dst_path / "submitted").write_text(wu + "\n")

        status["bundle_names"].append(dst_name)
        submitted_now += 1
        collect_and_purge()

    for doc_id, _ in chunk_rows:
        for p in (
            f"{scans_dir}/{doc_id}.jpg",
            f"{scans_dir}/{doc_id}.xml",
            f"{texts_dir}/{doc_id}.json",
            f"{stage_dir}/pages/{doc_id}",
        ):
            if os.path.isdir(p):
                shutil.rmtree(p, ignore_errors=True)
            elif os.path.exists(p):
                try:
                    os.remove(p)
                except OSError:
                    pass
    shutil.rmtree(f"{stage_dir}/units", ignore_errors=True)
    os.makedirs(f"{stage_dir}/units", exist_ok=True)
    return submitted_now


def discover_all_corpus_batches(con):
    """
    Discovers all 3,011 batches across the 23.77M-page corpus in priority order:
      Tier 0: Batches in wave9_manifest.parquet (229 batches, ordered by page count DESC).
      Tier 1: Additional batches with archived scans in usnmscansstprodffapzy/scans/keep/<batch>/.
      Tier 2: All remaining batches in stusnmdffapzy/curated/audit/damage/v2/<batch>_ver<NN>.parquet.
    Returns list of (batch, bv, est_pages, tier).
    """
    tier0 = con.execute(f"""
        SELECT batch, CAST(batch_version AS INTEGER) AS bv, COUNT(*) AS n
        FROM read_parquet('{SHARED_MANIFEST}')
        GROUP BY 1, 2
        ORDER BY n DESC, batch ASC, bv ASC
    """).fetchall()

    seen = {(b, int(bv)) for b, bv, _ in tier0}
    ordered = [(b, int(bv), int(n), 0) for b, bv, n in tier0]

    try:
        keep_prefixes = {
            p.rstrip("/").split("/")[-1]
            for p in list_blobs(SCANS_BASE, SCANS_SAS, "keep/", delimiter="/")
        }
    except Exception:
        keep_prefixes = set()

    try:
        dmg_blobs = sorted(list_blobs(CURATED_BASE, CURATED_SAS, "audit/damage/v2/"))
    except Exception:
        dmg_blobs = []

    tier1 = []
    tier2 = []
    pat = re.compile(r"^audit/damage/v2/(.+)_ver(\d+)\.parquet$")
    for bname in dmg_blobs:
        m = pat.match(bname)
        if not m:
            continue
        batch, bv_s = m.group(1), m.group(2)
        bv = int(bv_s)
        if (batch, bv) in seen:
            continue
        seen.add((batch, bv))
        if batch in keep_prefixes:
            tier1.append((batch, bv, 0, 1))
        else:
            tier2.append((batch, bv, 0, 2))

    ordered.extend(tier1)
    ordered.extend(tier2)
    return ordered


def load_batch_records(
    con, batch, bv, tier, work_dir, already_docs, pass_mode="archived", cached_jpeg_batches=None
):
    """
    Loads unbundled page records for (batch, bv):
    - pass_mode == "jpeg_cached": only pages already pre-converted in quality-jpeg/l-q75-r1/keep/<batch>/
    - pass_mode == "archived": only pages with archived blobs in usnmscansstprodffapzy/scans/keep/<batch>/
    - pass_mode == "loc_fallback": all remaining unbundled pages (including LoC tile.loc.gov fallback)
    """
    archived_only = pass_mode in ("jpeg_cached", "archived")
    cached_jp2s = set()
    jc_pfx = JPEG_CACHE_PREFIX.rstrip("/")
    if archived_only and (cached_jpeg_batches is None or batch in cached_jpeg_batches):
        try:
            for b in list_blobs(HANDOFF_BASE, HANDOFF_SAS, f"{jc_pfx}/keep/{batch}/"):
                if b.endswith(".jpg"):
                    cached_jp2s.add(b.removeprefix(f"{jc_pfx}/")[:-4])
        except Exception:
            pass
    if pass_mode == "jpeg_cached" and not cached_jp2s:
        return []

    cols = [
        "doc_id", "batch", "batch_version", "lccn", "date",
        "edition", "seq", "lang", "decision", "damage_rate", "blob_name",
    ]
    by_doc = {}
    if tier == 0:
        rows_raw = con.execute(f"""
            SELECT {', '.join(cols)}
            FROM read_parquet('{SHARED_MANIFEST}')
            WHERE batch = ? AND CAST(batch_version AS INTEGER) = ?
            ORDER BY doc_id
        """, [batch, int(bv)]).fetchall()
        for r in rows_raw:
            if r[0] not in already_docs:
                rec = dict(zip(cols, r))
                bname = rec.get("blob_name") or ""
                if pass_mode == "jpeg_cached" and bname not in cached_jp2s:
                    continue
                if archived_only and not bname:
                    continue
                by_doc[r[0]] = rec
        if by_doc or archived_only:
            records = list(by_doc.values())
            records.sort(
                key=lambda x: (
                    0 if x.get("blob_name") in cached_jp2s else (1 if x.get("blob_name") else 2),
                    x["doc_id"],
                )
            )
            return records

    if archived_only and tier == 2:
        return []

    keep_blobs = {}
    try:
        for bpath in list_blobs(SCANS_BASE, SCANS_SAS, f"keep/{batch}/"):
            if bpath.endswith((".jp2", ".jpg")):
                # keep/<batch>/<lccn>/<date>/<edition>/<seq>.(jp2|jpg)
                segs = bpath.split("/")
                if len(segs) == 6:
                    _, _, lccn_s, date_s, ed_s, seq_ext = segs
                    seq_s = seq_ext.rsplit(".", 1)[0]
                    did = f"{lccn_s}_{date_s}_ed-{ed_s}_seq-{seq_s}"
                    if did not in keep_blobs or bpath.endswith(".jpg"):
                        keep_blobs[did] = bpath
    except Exception:
        pass

    if archived_only and not any(did not in already_docs for did in keep_blobs):
        return []

    # Full-corpus batch discovery via audit/damage/v2/<batch>_ver<bv:02d>.parquet
    dmg_blob = f"audit/damage/v2/{batch}_ver{int(bv):02d}.parquet"
    tmp_dmg = f"{work_dir}/tmp_dmg.parquet"
    if not download_blob(f"{CURATED_BASE}/{dmg_blob}?{CURATED_SAS}", tmp_dmg):
        return list(by_doc.values())

    try:
        d_rows = con.execute(f"""
            SELECT
                doc_id,
                batch,
                CAST(batch_version AS INTEGER) AS batch_version,
                lccn,
                CAST(date AS VARCHAR) AS date,
                CAST(edition AS INTEGER) AS edition,
                CAST(seq AS INTEGER) AS seq,
                COALESCE(lang, 'eng') AS lang,
                COALESCE(decision, 'detected') AS decision,
                COALESCE(damage_rate, 0.15) AS damage_rate
            FROM read_parquet('{tmp_dmg}')
            ORDER BY doc_id
        """).fetchall()

        meta_rows = []
        for r in d_rows:
            did = r[0]
            if did in already_docs or did in by_doc:
                continue
            bpath = keep_blobs.get(did, "")
            if pass_mode == "jpeg_cached" and bpath not in cached_jp2s:
                continue
            if archived_only and not bpath:
                continue
            rec = {
                "doc_id": did,
                "batch": r[1],
                "batch_version": r[2],
                "lccn": r[3],
                "date": r[4],
                "edition": r[5],
                "seq": r[6],
                "lang": r[7],
                "decision": r[8],
                "damage_rate": r[9],
                "blob_name": bpath,
            }
            by_doc[did] = rec
            meta_rows.append((did, r[1], int(r[2]), str(r[4])))

        if meta_rows:
            meta_pq = Path(STAGED_META_DIR) / f"{batch}_ver{int(bv):02d}.parquet"
            if not meta_pq.exists():
                try:
                    tmp_m = f"{work_dir}/tmp_meta.parquet"
                    con.executemany(
                        "CREATE TEMP TABLE IF NOT EXISTS _bmeta (doc_id VARCHAR, batch VARCHAR, batch_version BIGINT, date VARCHAR); INSERT INTO _bmeta VALUES (?, ?, ?, ?);",
                        meta_rows[:1],
                    )
                    con.execute("DELETE FROM _bmeta;")
                    con.executemany("INSERT INTO _bmeta VALUES (?, ?, ?, ?);", meta_rows)
                    con.execute(f"COPY _bmeta TO '{tmp_m}' (FORMAT PARQUET, COMPRESSION ZSTD);")
                    shutil.copyfile(tmp_m, meta_pq)
                    os.remove(tmp_m)
                except Exception:
                    pass
    finally:
        if os.path.exists(tmp_dmg):
            os.remove(tmp_dmg)

    records = list(by_doc.values())
    # Prioritize pre-cached JPEGs first, then archived scans, then LoC fallback pages
    records.sort(
        key=lambda x: (
            0 if x.get("blob_name") in cached_jp2s else (1 if x.get("blob_name") else 2),
            x["doc_id"],
        )
    )
    return records


def process_shard(shard_idx):
    import duckdb
    from PIL import Image
    Image.MAX_IMAGE_PIXELS = None

    t0 = time.time()
    con = duckdb.connect()
    con.execute("SET memory_limit='512MB'; SET threads=2;")
    already_docs, max_seq = load_all_existing_bundled_docs(shard_idx)
    bundle_seq = max_seq + 1

    all_batches = discover_all_corpus_batches(con)
    my_batches = [b for i, b in enumerate(all_batches) if (i % NUM_SHARDS) == shard_idx]
    tier0_target_pages = sum(b[2] for b in my_batches if b[3] == 0)

    work_dir = f"/tmp/w9-s{shard_idx}"
    scans_dir = f"{work_dir}/scans"
    texts_dir = f"{work_dir}/texts"
    stage_dir = f"{work_dir}/stage"
    shutil.rmtree(work_dir, ignore_errors=True)
    for d in (scans_dir, texts_dir, f"{stage_dir}/units", f"{stage_dir}/pages"):
        os.makedirs(d, exist_ok=True)

    status = {
        "shard": shard_idx,
        "num_shards": NUM_SHARDS,
        "worker_version": "1.3",
        "engine_image": ENGINE_IMAGE,
        "bundle_size": BUNDLE_SIZE,
        "max_in_flight_bundles": MAX_IN_FLIGHT_BUNDLES,
        "state": "running",
        "total_corpus_batches": len(all_batches),
        "assigned_batches": len(my_batches),
        "completed_batches": 0,
        "current_batch": None,
        "current_tier": 0,
        "tier0_target_pages": tier0_target_pages,
        "already_bundled_total": len(already_docs),
        "staged_pages": 0,
        "bundles_submitted": max_seq,
        "jpeg_cache_hits": 0,
        "jpeg_cache_misses": 0,
        "bundle_names": [],
        "skipped_errors": [],
        "elapsed_s": 0.0,
    }
    put_status(f"shard-{shard_idx}.json", status)

    ocr_local_lccns = {
        p.rstrip("/").split("/")[-1]
        for p in list_blobs(CURATED_BASE, CURATED_SAS, "ocr-local/pages/", delimiter="/")
    }
    as_lccn_blobs = {}
    staged_rows = []
    cache_lock = threading.Lock()

    for pass_mode in ("jpeg_cached", "jpeg_cached", "archived", "loc_fallback"):
        archived_only = pass_mode != "loc_fallback"
        status["current_pass"] = pass_mode
        jc_pfx = JPEG_CACHE_PREFIX.rstrip("/")
        try:
            cached_jpeg_batches = {
                p.rstrip("/").split("/")[-1]
                for p in list_blobs(HANDOFF_BASE, HANDOFF_SAS, f"{jc_pfx}/keep/", delimiter="/")
            }
        except Exception:
            cached_jpeg_batches = None
        for b_idx, (batch, bv, _, tier) in enumerate(my_batches, 1):
            touch_shard_heartbeat(shard_idx)
            batch_records = load_batch_records(
                con,
                batch,
                bv,
                tier,
                work_dir,
                already_docs,
                pass_mode=pass_mode,
                cached_jpeg_batches=cached_jpeg_batches,
            )
            if not batch_records:
                if not archived_only:
                    status["completed_batches"] = b_idx
                continue

            status["current_batch"] = f"{batch}_ver{int(bv):02d}"
            status["current_tier"] = tier
            status["elapsed_s"] = round(time.time() - t0, 1)
            put_status(f"shard-{shard_idx}.json", status)

            batch_doc_set = {r["doc_id"] for r in batch_records}
            loc_resolver = LocBatchResolver(batch, bv)

            # 1. Load LoC OCR texts from curated/pages/<batch>/v<bv:02d>/
            loc_map = {}
            page_pfx = f"pages/{batch}/v{int(bv):02d}/"
            for bname in sorted(list_blobs(CURATED_BASE, CURATED_SAS, page_pfx)):
                if not bname.endswith(".parquet"):
                    continue
                tmp_pq = f"{work_dir}/tmp_loc.parquet"
                if download_blob(f"{CURATED_BASE}/{bname}?{CURATED_SAS}", tmp_pq):
                    try:
                        for did, src, txt in con.execute(
                            f"SELECT doc_id, ocr_source, text FROM read_parquet('{tmp_pq}')"
                        ).fetchall():
                            if did in batch_doc_set and txt:
                                loc_map[did] = (src or "ndnp-original", txt)
                    except Exception as e:
                        status["skipped_errors"].append(f"loc pq {bname}: {str(e)[:120]}")
                    finally:
                        if os.path.exists(tmp_pq):
                            os.remove(tmp_pq)

            no_loc_docs = [r["doc_id"] for r in batch_records if r["doc_id"] not in loc_map]
            if no_loc_docs:
                already_docs.update(no_loc_docs)
                record_skipped_docs(no_loc_docs)

            valid_records = [r for r in batch_records if r["doc_id"] in loc_map]
            if not valid_records:
                if not archived_only:
                    status["completed_batches"] = b_idx
                continue

            # 2. Load auxiliary readers (AS + ocr-local) scoped strictly to valid_records
            valid_doc_set = {r["doc_id"] for r in valid_records}
            ours_map = {}
            alto_blob_map = {}
            as_map = {}
            by_lccn_years = {}
            for r in valid_records:
                yr = str(r["date"])[:4]
                by_lccn_years.setdefault(r["lccn"], set()).add(yr)

            for lccn, yrs in by_lccn_years.items():
                if lccn in ocr_local_lccns:
                    for bname in list_blobs(CURATED_BASE, CURATED_SAS, f"ocr-local/pages/{lccn}/"):
                        if not bname.endswith(".parquet"):
                            continue
                        tmp_pq = f"{work_dir}/tmp_ol.parquet"
                        if download_blob(f"{CURATED_BASE}/{bname}?{CURATED_SAS}", tmp_pq):
                            try:
                                for did, eng, src, txt in con.execute(
                                    f"SELECT doc_id, ocr_engine, ocr_source, text FROM read_parquet('{tmp_pq}')"
                                ).fetchall():
                                    if did in valid_doc_set and txt:
                                        ours_map[did] = (eng or "", src or "", txt)
                            except Exception:
                                pass
                            finally:
                                if os.path.exists(tmp_pq):
                                    os.remove(tmp_pq)
                    for bname in list_blobs(CURATED_BASE, CURATED_SAS, f"ocr-local/alto/{lccn}"):
                        if bname.endswith(".xml"):
                            did = os.path.basename(bname).split(".")[0]
                            if did in valid_doc_set:
                                alto_blob_map[did] = bname

                if lccn not in as_lccn_blobs:
                    as_lccn_blobs[lccn] = list_blobs(
                        CURATED_BASE, CURATED_SAS, f"american-stories/pages/{lccn}/"
                    )
                want_as_blobs = []
                for yr in sorted(yrs):
                    pfx = f"american-stories/pages/{lccn}/{yr}-"
                    for bname in as_lccn_blobs[lccn]:
                        if bname.startswith(pfx) and bname.endswith(".parquet"):
                            want_as_blobs.append(bname)
                if want_as_blobs:
                    as_tmp_dir = f"{work_dir}/tmp_as_dir"
                    shutil.rmtree(as_tmp_dir, ignore_errors=True)
                    os.makedirs(as_tmp_dir, exist_ok=True)
                    def _dl_as(idx_bname):
                        idx, bn = idx_bname
                        dst = f"{as_tmp_dir}/{idx:05d}.parquet"
                        return dst if download_blob(f"{CURATED_BASE}/{bn}?{CURATED_SAS}", dst) else None
                    with ThreadPoolExecutor(max_workers=16) as as_ex:
                        as_files = [p for p in as_ex.map(_dl_as, enumerate(want_as_blobs)) if p]
                    if as_files:
                        try:
                            for did, txt in con.execute(
                                f"SELECT doc_id, text FROM read_parquet({as_files})"
                            ).fetchall():
                                if did in valid_doc_set and txt:
                                    as_map[did] = txt
                        except Exception:
                            pass
                    shutil.rmtree(as_tmp_dir, ignore_errors=True)

            def dl_and_prepare_page(r, loc_entry):
                did = r["doc_id"]
                blob_name = r.get("blob_name") or ""
                jpg_dst = f"{scans_dir}/{did}.jpg"
                xml_dst = f"{scans_dir}/{did}.xml"

                try:
                    loc_alto_url = None
                    cached = cached_page(blob_name) if (blob_name and blob_name.endswith(".jp2")) else None
                    from_cache = cached is not None
                    if from_cache:
                        with open(jpg_dst, "wb") as f:
                            f.write(cached)
                        with cache_lock:
                            status["jpeg_cache_hits"] = status.get("jpeg_cache_hits", 0) + 1
                    elif blob_name:
                        scan_url = f"{SCANS_BASE}/{blob_name}?{SCANS_SAS}"
                        raw = download_bytes(scan_url)
                        is_jp2 = blob_name.endswith(".jp2")
                        if is_jp2:
                            with cache_lock:
                                status["jpeg_cache_misses"] = status.get("jpeg_cache_misses", 0) + 1
                    else:
                        jp2_url, loc_alto_url = loc_resolver.resolve(
                            r["lccn"], r["date"], r["edition"], r["seq"]
                        )
                        if not jp2_url:
                            raise RuntimeError(f"no archive or LoC bag entry for {did}")
                        raw = loc_paced_download(jp2_url)
                        is_jp2 = True

                    w, h = 0, 0
                    if from_cache:
                        with Image.open(io.BytesIO(cached)) as im:
                            w, h = im.size
                    elif is_jp2 or len(raw) > 20_000_000:
                        with Image.open(io.BytesIO(raw)) as im:
                            if getattr(im, "format", "") == "JPEG2000" and max(im.size) > 4000:
                                im.reduce = 1
                            im_l = im.convert("L")
                            w, h = im_l.size
                            im_l.save(jpg_dst, "JPEG", quality=75)
                            im_l.close()
                    else:
                        with Image.open(io.BytesIO(raw)) as im:
                            w, h = im.size
                            if w * h > 40_000_000 or im.mode != "L":
                                im_l = im.convert("L")
                                w, h = im_l.size
                                im_l.save(jpg_dst, "JPEG", quality=75)
                                im_l.close()
                            else:
                                with open(jpg_dst, "wb") as f:
                                    f.write(raw)
                    del cached
                    if not from_cache:
                        del raw

                    xml_blob = alto_blob_map.get(did)
                    got_alto = False
                    if xml_blob and download_blob(f"{CURATED_BASE}/{xml_blob}?{CURATED_SAS}", xml_dst):
                        got_alto = True
                    elif loc_alto_url:
                        try:
                            alto_bytes = loc_paced_download(loc_alto_url)
                            with open(xml_dst, "wb") as f:
                                f.write(alto_bytes)
                            got_alto = True
                        except Exception:
                            got_alto = False
                    if not got_alto:
                        write_empty_alto(xml_dst, w, h)

                    _, loc_txt = loc_entry
                    loc_norm = normalize_text(loc_txt)
                    if not loc_norm.strip():
                        loc_norm = " "
                    readers = [{
                        "alto": False,
                        "name": "loc-hist",
                        "regions": [],
                        "source": "loc-hist",
                        "text": loc_norm,
                    }]
                    if did in as_map:
                        as_norm = normalize_text(as_map[did])
                        if as_norm.strip():
                            readers.append({
                                "alto": False,
                                "name": "as",
                                "regions": [],
                                "source": "american-stories",
                                "text": as_norm,
                            })
                    if did in ours_map:
                        eng, src, otxt = ours_map[did]
                        onorm = normalize_text(otxt)
                        if onorm.strip():
                            low = f"{eng} {src}".lower()
                            rname = "ours-surya" if "surya" in low else (
                                "ours-ndlocr" if "ndlocr" in low else (
                                    "ours-paddle" if "paddle" in low else "ours-tesseract"
                                )
                            )
                            readers.append({
                                "alto": False,
                                "name": rname,
                                "regions": [],
                                "source": "ocr-local:tesseract",
                                "text": onorm,
                            })
                    with open(f"{texts_dir}/{did}.json", "w") as f:
                        json.dump({"page": None, "readers": readers}, f)
                    return did, True, None
                except Exception as e:
                    for p in (jpg_dst, xml_dst, f"{texts_dir}/{did}.json"):
                        if os.path.exists(p):
                            try:
                                os.remove(p)
                            except OSError:
                                pass
                    return did, False, str(e)

            pos = 0
            while pos < len(valid_records):
                touch_shard_heartbeat(shard_idx)
                need = max(1, BUNDLE_SIZE - len(staged_rows))
                slice_records = valid_records[pos : pos + need]
                pos += len(slice_records)

                # Use 8 threads when slices are pre-cached JPEGs, 2 threads for raw JP2 decode, 1 thread for LoC
                has_unarchived = any(not r.get("blob_name") for r in slice_records)
                workers = 1 if has_unarchived else (8 if pass_mode == "jpeg_cached" else 2)

                ok_docs = set()
                failed_docs = []
                with ThreadPoolExecutor(max_workers=workers) as ex:
                    futs = [
                        ex.submit(dl_and_prepare_page, r, loc_map[r["doc_id"]])
                        for r in slice_records
                    ]
                    for fut in as_completed(futs):
                        did, ok, err = fut.result()
                        if ok:
                            ok_docs.add(did)
                        else:
                            failed_docs.append(did)
                            if len(status["skipped_errors"]) < 200:
                                status["skipped_errors"].append(f"{did}: {err[:120] if err else 'dl failed'}")

                if failed_docs:
                    already_docs.update(failed_docs)
                    record_skipped_docs(failed_docs)

                for r in slice_records:
                    if r["doc_id"] in ok_docs:
                        staged_rows.append((r["doc_id"], r))
                        already_docs.add(r["doc_id"])
                        status["staged_pages"] += 1

                status["elapsed_s"] = round(time.time() - t0, 1)
                put_status(f"shard-{shard_idx}.json", status)
                gc.collect()

                while len(staged_rows) >= BUNDLE_SIZE:
                    chunk = staged_rows[:BUNDLE_SIZE]
                    staged_rows = staged_rows[BUNDLE_SIZE:]
                    n_sub = flush_bundle(
                        shard_idx, chunk, bundle_seq, stage_dir, scans_dir, texts_dir, status
                    )
                    status["bundles_submitted"] += n_sub
                    bundle_seq += max(1, n_sub)
                    status["elapsed_s"] = round(time.time() - t0, 1)
                    put_status(f"shard-{shard_idx}.json", status)

            if not archived_only:
                status["completed_batches"] = b_idx
            status["elapsed_s"] = round(time.time() - t0, 1)
            put_status(f"shard-{shard_idx}.json", status)

    if staged_rows:
        n_sub = flush_bundle(
            shard_idx, staged_rows, bundle_seq, stage_dir, scans_dir, texts_dir, status
        )
        status["bundles_submitted"] += n_sub
        staged_rows = []

    Path(f"{LOCKS_DIR}/shard-{shard_idx}.done").write_text("ok\n")
    status["state"] = "done"
    status["elapsed_s"] = round(time.time() - t0, 1)
    put_status(f"shard-{shard_idx}.json", status)
    shutil.rmtree(work_dir, ignore_errors=True)


def claim_shard_lock(shard_idx):
    os.makedirs(LOCAL_LOCKS_DIR, exist_ok=True)
    lock_dir = f"{LOCAL_LOCKS_DIR}/shard-{shard_idx}.lock"
    hb_file = f"{lock_dir}/heartbeat"
    try:
        os.mkdir(lock_dir)
        Path(hb_file).write_text(f"{time.time()}\n")
        return True
    except OSError:
        return False


def release_shard_lock(shard_idx):
    shutil.rmtree(f"{LOCAL_LOCKS_DIR}/shard-{shard_idx}.lock", ignore_errors=True)


def drain_until_all_collected():
    while True:
        c = collect_and_purge(force=True)
        if c and c.get("pending_uncollected_bundles", 1) == 0:
            break
        time.sleep(30)


def main():
    try:
        bootstrap_runtime()
        free_assimilated_downloads()
        init_wave9()
        preferred = os.environ.get("JOB_COMPLETION_INDEX") or os.environ.get("SHARD_ENV")
        shard_order = list(range(NUM_SHARDS))
        if preferred is not None and str(preferred).isdigit():
            p_idx = int(preferred) % NUM_SHARDS
            shard_order = [p_idx] + [s for s in range(NUM_SHARDS) if s != p_idx]

        for s in shard_order:
            if os.path.exists(f"{LOCKS_DIR}/shard-{s}.done"):
                continue
            if not claim_shard_lock(s):
                continue
            try:
                process_shard(s)
            finally:
                release_shard_lock(s)
        drain_until_all_collected()
    except Exception:
        tb = traceback.format_exc()
        print(tb, file=sys.stderr, flush=True)
        put_status(f"error-{os.getpid()}.json", {"ts": time.time(), "tb": tb})
        sys.exit(1)


if __name__ == "__main__":
    main()
