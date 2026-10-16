#!/usr/bin/env python3
"""Idempotent server-side GPU queue provisioner, CIFS nbytes repairer, and status probe."""
import hashlib
import json
import os
import re
import shutil
import subprocess
import time
import urllib.request
from pathlib import Path

MYCNF = "/dev/shm/boinc/mysql.cnf"
DB = os.environ.get("DB_NAME", "usnmocr")
PROJ = Path("/project/usnmocr")
SERVER_REPLICA = os.environ.get("SERVER_REPLICA_HOST", "usnm-boinc-ca-prod--0000046-d7b58fbfc-cslk6")

WEIGHTS_LOCK = """# Qwen3-VL's weights in the usnm_qwen worker image (#522): <path under /opt/qwen/models> <URL> <sha256>.
LICENSE https://www.apache.org/licenses/LICENSE-2.0.txt cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30
Qwen3VL-8B-Instruct-Q4_K_M.gguf https://huggingface.co/Qwen/Qwen3-VL-8B-Instruct-GGUF/resolve/f982a07559d4a2f6c8744d840bf6fccab30eea96/Qwen3VL-8B-Instruct-Q4_K_M.gguf 67d1659bfe71b89d50b45a4ad1a9e5b997e5bb16ce5da66a6a6167abd569e9e2
mmproj-Qwen3VL-8B-Instruct-Q8_0.gguf https://huggingface.co/Qwen/Qwen3-VL-8B-Instruct-GGUF/resolve/f982a07559d4a2f6c8744d840bf6fccab30eea96/mmproj-Qwen3VL-8B-Instruct-Q8_0.gguf c6ba85508d82f42590e6eb77d5340369ab6fecf107a7561d809523d8aa5f3bfd
Qwen3-VL-2B-Instruct-Q8_0.gguf https://huggingface.co/ggml-org/Qwen3-VL-2B-Instruct-GGUF/resolve/ea6a11058182570be6436b9a2e4ee7f7b49f908d/Qwen3-VL-2B-Instruct-Q8_0.gguf b7802e29f71a9e5b5e3f83f613df898a2204342dcea71a231ea501d481813c39
mmproj-Qwen3-VL-2B-Instruct-Q8_0.gguf https://huggingface.co/ggml-org/Qwen3-VL-2B-Instruct-GGUF/resolve/ea6a11058182570be6436b9a2e4ee7f7b49f908d/mmproj-Qwen3-VL-2B-Instruct-Q8_0.gguf 69066c8f279ec85ff48ab4059f6ebba0d2932ca57667f2bbdac7d9805bca9e7b
"""

TEMPLATE_QWEN_IN = """<input_template>
  <file_info><number>0</number></file_info>
  <file_info><number>1</number></file_info>
  <workunit>
    <file_ref><file_number>0</file_number><open_name>page.jpg</open_name><copy_file/></file_ref>
    <file_ref><file_number>1</file_number><open_name>qwen.json</open_name><copy_file/></file_ref>
  </workunit>
</input_template>
"""

TEMPLATE_QWEN_8B_IN = """<input_template>
    <file_info>
        <number>0</number>
    </file_info>
    <file_info>
        <number>1</number>
    </file_info>
    <file_info>
        <number>2</number>
        <sticky/>
        <no_delete/>
    </file_info>
    <file_info>
        <number>3</number>
        <sticky/>
        <no_delete/>
    </file_info>
    <workunit>
        <file_ref>
            <file_number>0</file_number>
            <open_name>page.jpg</open_name>
            <copy_file/>
        </file_ref>
        <file_ref>
            <file_number>1</file_number>
            <open_name>qwen.json</open_name>
            <copy_file/>
        </file_ref>
        <file_ref>
            <file_number>2</file_number>
            <open_name>Qwen3VL-8B-Instruct-Q4_K_M.gguf</open_name>
        </file_ref>
        <file_ref>
            <file_number>3</file_number>
            <open_name>mmproj-Qwen3VL-8B-Instruct-Q8_0.gguf</open_name>
        </file_ref>
        <rsc_fpops_est>1e13</rsc_fpops_est>
        <rsc_fpops_bound>1e16</rsc_fpops_bound>
        <rsc_memory_bound>6442450944</rsc_memory_bound>
        <rsc_disk_bound>19327352832</rsc_disk_bound>
        <delay_bound>259200</delay_bound>
        <min_quorum>2</min_quorum>
        <target_nresults>2</target_nresults>
        <max_error_results>3</max_error_results>
        <max_total_results>4</max_total_results>
        <max_success_results>3</max_success_results>
    </workunit>
</input_template>
"""

TEMPLATE_QWEN_2B_IN = """<input_template>
    <file_info>
        <number>0</number>
    </file_info>
    <file_info>
        <number>1</number>
    </file_info>
    <file_info>
        <number>2</number>
        <sticky/>
        <no_delete/>
    </file_info>
    <file_info>
        <number>3</number>
        <sticky/>
        <no_delete/>
    </file_info>
    <workunit>
        <file_ref>
            <file_number>0</file_number>
            <open_name>page.jpg</open_name>
            <copy_file/>
        </file_ref>
        <file_ref>
            <file_number>1</file_number>
            <open_name>qwen.json</open_name>
            <copy_file/>
        </file_ref>
        <file_ref>
            <file_number>2</file_number>
            <open_name>Qwen3-VL-2B-Instruct-Q8_0.gguf</open_name>
        </file_ref>
        <file_ref>
            <file_number>3</file_number>
            <open_name>mmproj-Qwen3-VL-2B-Instruct-Q8_0.gguf</open_name>
        </file_ref>
        <rsc_fpops_est>5e12</rsc_fpops_est>
        <rsc_fpops_bound>1e16</rsc_fpops_bound>
        <rsc_memory_bound>4294967296</rsc_memory_bound>
        <rsc_disk_bound>12884901888</rsc_disk_bound>
        <delay_bound>259200</delay_bound>
        <min_quorum>2</min_quorum>
        <target_nresults>2</target_nresults>
        <max_error_results>3</max_error_results>
        <max_total_results>4</max_total_results>
        <max_success_results>3</max_success_results>
    </workunit>
</input_template>
"""

TEMPLATE_QWEN_OUT = """<output_template>
  <file_info>
    <name><OUTFILE_0/></name>
    <generated_locally/>
    <upload_when_present/>
    <max_nbytes>2097152</max_nbytes>
    <url><UPLOAD_URL/></url>
  </file_info>
  <file_info>
    <name><OUTFILE_1/></name>
    <generated_locally/>
    <upload_when_present/>
    <max_nbytes>65536</max_nbytes>
    <url><UPLOAD_URL/></url>
  </file_info>
  <file_info>
    <name><OUTFILE_2/></name>
    <generated_locally/>
    <upload_when_present/>
    <max_nbytes>65536</max_nbytes>
    <url><UPLOAD_URL/></url>
  </file_info>
  <result>
    <file_ref><file_name><OUTFILE_0/></file_name><open_name>readings.json</open_name><copy_file/></file_ref>
    <file_ref><file_name><OUTFILE_1/></file_name><open_name>result.json</open_name><copy_file/></file_ref>
    <file_ref><file_name><OUTFILE_2/></file_name><open_name>diag.txt</open_name><copy_file/></file_ref>
  </result>
</output_template>
"""


def q(sql: str) -> list[dict]:
    out = subprocess.check_output(
        ["mysql", f"--defaults-extra-file={MYCNF}", "--ssl", "-B", DB, "-e", sql],
        text=True,
    )
    lines = [l for l in out.splitlines() if l.strip()]
    if not lines:
        return []
    hdr = lines[0].split("\t")
    return [dict(zip(hdr, l.split("\t"))) for l in lines[1:]]


def exec_sql(sql: str) -> None:
    subprocess.run(
        ["mysql", f"--defaults-extra-file={MYCNF}", "--ssl", DB, "-e", sql],
        check=True,
    )


def fanout_path(filename: str, fanout: int = 1024) -> Path:
    h = hashlib.md5(filename.encode("utf-8")).hexdigest()
    subdir = f"{int(h[:8], 16) % fanout:x}"
    return PROJ / "download" / subdir / filename


def write_fsync(dest: Path, data: bytes) -> None:
    dest.parent.mkdir(parents=True, exist_ok=True)
    with open(dest, "wb") as f:
        f.write(data)
        f.flush()
        os.fsync(f.fileno())


def copy_fsync(src: Path, dest: Path) -> None:
    dest.parent.mkdir(parents=True, exist_ok=True)
    with open(src, "rb") as fsrc, open(dest, "wb") as fdst:
        shutil.copyfileobj(fsrc, fdst, length=1024 * 1024)
        fdst.flush()
        os.fsync(fdst.fileno())


def _safe_mtime(p: Path) -> float:
    try:
        return p.lstat().st_mtime
    except OSError:
        return 0.0


def detect_active_server_replica() -> str:
    env_host = os.environ.get("SERVER_REPLICA_HOST", "").strip()
    if env_host:
        return env_host
    import socket
    hn = socket.gethostname()
    if hn.startswith("usnm-boinc-ca-prod"):
        return hn
    pid_dirs = sorted(
        [p for p in PROJ.glob("pid_usnm-boinc-ca-prod*") if p.exists()],
        key=_safe_mtime,
        reverse=True,
    )
    if pid_dirs:
        return pid_dirs[0].name.removeprefix("pid_")
    log_dirs = sorted(
        [p for p in PROJ.glob("log_usnm-boinc-ca-prod*") if p.exists()],
        key=_safe_mtime,
        reverse=True,
    )
    if log_dirs:
        return log_dirs[0].name.removeprefix("log_")
    return SERVER_REPLICA


def _patch_scheduler_config_xml(text: str, server_replica: str | None = None) -> str:
    if server_replica:
        text = re.sub(r"<host>[^<]+</host>", f"<host>{server_replica}</host>", text)
    text = re.sub(r"\s*<one_result_per_user_per_wu\s*/>", "", text)
    if "<enable_assignment>" not in text:
        text = text.replace(
            "<show_results>1</show_results>",
            "<show_results>1</show_results>\n        <enable_assignment>1</enable_assignment>",
        )
    text = re.sub(r"<min_sendwork_interval>\d+</min_sendwork_interval>", "<min_sendwork_interval>2</min_sendwork_interval>", text)
    text = re.sub(r"<max_wus_in_progress>\d+</max_wus_in_progress>", "<max_wus_in_progress>32</max_wus_in_progress>", text)
    text = re.sub(r"<daily_result_quota>\d+</daily_result_quota>", "<daily_result_quota>50000</daily_result_quota>", text)
    text = re.sub(r"<max_wus_to_send>\d+</max_wus_to_send>", "<max_wus_to_send>64</max_wus_to_send>", text)
    if "<resend_lost_results>" not in text:
        text = text.replace(
            "<max_wus_to_send>64</max_wus_to_send>",
            "<max_wus_to_send>64</max_wus_to_send>\n        <resend_lost_results>1</resend_lost_results>",
        )
    if "<cache_md5_info>" not in text:
        text = text.replace(
            "<resend_lost_results>1</resend_lost_results>",
            "<resend_lost_results>1</resend_lost_results>\n        <cache_md5_info>1</cache_md5_info>",
        )
    return text


def ensure_server_config_xml() -> dict:
    """Render /project/usnmocr/config.xml with the server replica's <host> and without <one_result_per_user_per_wu/>."""
    server_replica = detect_active_server_replica()
    cfg_path = PROJ / "config.xml"
    shm_cfg = Path("/dev/shm/boinc/config.xml")
    tpl_cfg = Path("/opt/boinc-templates/config.xml.template")
    before = os.readlink(cfg_path) if cfg_path.is_symlink() else ("file" if cfg_path.exists() else "missing")
    updated = False
    if tpl_cfg.is_file():
        try:
            tpl_text = _patch_scheduler_config_xml(tpl_cfg.read_text(errors="replace"))
            tpl_cfg.write_text(tpl_text)
        except OSError:
            pass
    if shm_cfg.is_file():
        text = _patch_scheduler_config_xml(shm_cfg.read_text(errors="replace"), server_replica)
        shm_cfg.write_text(text)
        mycnf_path = Path(MYCNF)
        if not mycnf_path.is_file():
            m_host = re.search(r"<db_host>([^<]+)</db_host>", text)
            m_user = re.search(r"<db_user>([^<]+)</db_user>", text)
            m_pass = re.search(r"<db_passwd>([^<]+)</db_passwd>", text)
            if m_host and m_user and m_pass:
                mycnf_path.parent.mkdir(parents=True, exist_ok=True)
                mycnf_path.write_text(
                    f"[client]\nhost={m_host.group(1).strip()}\nuser={m_user.group(1).strip()}\npassword=\"{m_pass.group(1).strip()}\"\n"
                )
                os.chmod(mycnf_path, 0o600)
        if cfg_path.is_symlink():
            cfg_path.unlink()
            updated = True
        if not cfg_path.is_file() or cfg_path.read_text(errors="replace") != text:
            write_fsync(cfg_path, text.encode("utf-8"))
            os.chmod(cfg_path, 0o640)
            updated = True
    return {"updated": updated, "before_host": before, "after_host": server_replica}


def repair_cifs_nbytes_in_mysql() -> dict:
    """Find any unfinished Wave 5 workunits whose <file_info> <nbytes>/<md5_cksum> was truncated at a CIFS boundary."""
    marker = Path("/project/quality/status/cifs-nbytes-repaired-v2.json")
    where_clause = (
        "wu.canonical_resultid = 0 AND (wu.xml_doc LIKE '%<nbytes>0</nbytes>%' OR wu.xml_doc LIKE '%d41d8cd98f00b204e9800998ecf8427e%')"
        if marker.is_file()
        else "wu.id < 8000 AND wu.canonical_resultid = 0"
    )
    rows = q(
        f"""
        SELECT wu.id, wu.name, REPLACE(REPLACE(wu.xml_doc, '\\n', ' '), '\\t', ' ') AS xml_flat
        FROM workunit wu
        WHERE {where_clause}
        """
    )
    checked_files = 0
    repaired_wus = 0
    repaired_files = []
    file_info_block_re = re.compile(r"<file_info>.*?</file_info>", re.DOTALL)
    empty_md5 = "d41d8cd98f00b204e9800998ecf8427e"

    for r in rows:
        wu_id = int(r["id"])
        xml_flat = r.get("xml_flat", "")
        blocks = file_info_block_re.findall(xml_flat)
        needs_fix = False
        fixes = []
        for block in blocks:
            m_name = re.search(r"<name>([^<]+)</name>", block)
            m_nb = re.search(r"<nbytes>(\d+)</nbytes>", block)
            m_md5 = re.search(r"<md5_cksum>([0-9a-fA-F]+)</md5_cksum>", block)
            if not (m_name and m_nb and m_md5):
                continue
            fname = m_name.group(1).strip()
            nb = int(m_nb.group(1))
            md5_str = m_md5.group(1).strip()
            if nb == 0 or nb % 4096 == 0 or md5_str.lower() == empty_md5:
                checked_files += 1
                fpath = fanout_path(fname)
                if fpath.is_file():
                    st_size = fpath.stat().st_size
                    if st_size != nb or md5_str.lower() == empty_md5:
                        needs_fix = True
                        real_bytes = fpath.read_bytes()
                        real_md5 = hashlib.md5(real_bytes).hexdigest()
                        fixes.append((fname, nb, len(real_bytes), md5_str, real_md5))
        if needs_fix:
            exact_out = subprocess.check_output(
                [
                    "mysql",
                    f"--defaults-extra-file={MYCNF}",
                    "--ssl",
                    "-N",
                    "-B",
                    "-r",
                    DB,
                    "-e",
                    f"SELECT xml_doc FROM workunit WHERE id = {wu_id}",
                ],
                text=True,
            )
            xml_doc = exact_out.rstrip("\n")
            for fname, old_nb, new_nb, old_md5, new_md5 in fixes:
                def _fix_block(m: re.Match) -> str:
                    b = m.group(0)
                    if f"<name>{fname}</name>" not in b:
                        return b
                    b = re.sub(r"<nbytes>\d+</nbytes>", f"<nbytes>{new_nb}</nbytes>", b, count=1)
                    b = re.sub(r"<md5_cksum>[0-9a-fA-F]+</md5_cksum>", f"<md5_cksum>{new_md5}</md5_cksum>", b, count=1)
                    return b
                xml_doc = file_info_block_re.sub(_fix_block, xml_doc)
                repaired_files.append({"wu_id": wu_id, "file": fname, "old_nbytes": old_nb, "new_nbytes": new_nb})
            xml_hex = xml_doc.encode("utf-8").hex()
            exec_sql(f"UPDATE workunit SET xml_doc = UNHEX('{xml_hex}') WHERE id = {wu_id}")
            repaired_wus += 1

    if repaired_wus > 0 or not marker.is_file():
        exec_sql(
            """
            UPDATE result r
            JOIN workunit wu ON r.workunitid = wu.id
            SET r.server_state = 2,
                r.outcome = 0,
                r.client_state = 0,
                r.validate_state = 0,
                r.hostid = 0,
                r.userid = 0,
                r.sent_time = 0,
                r.received_time = 0,
                r.stderr_out = '',
                wu.error_mask = (wu.error_mask & ~2),
                wu.transition_time = UNIX_TIMESTAMP()
            WHERE wu.id < 8000
              AND wu.canonical_resultid = 0
              AND r.server_state = 5
              AND r.outcome = 3
              AND r.client_state = 1
            """
        )

    res = {
        "unsent_wus_scanned": len(rows),
        "cifs_boundary_files_checked": checked_files,
        "repaired_wus": repaired_wus,
        "repaired_files_sample": repaired_files[:20],
    }
    if not marker.is_file():
        write_fsync(marker, json.dumps(res, indent=2).encode("utf-8"))
    return res


def ensure_sticky_gguf_models() -> list[dict]:
    """Ensure the 4 sticky GGUF models from weights.lock exist in /project/usnmocr/download/<fanout>/."""
    results = []
    for line in WEIGHTS_LOCK.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        logical, url, expected_sha = line.split()
        if not logical.endswith(".gguf"):
            continue
        physical = f"usnm_qwen_{expected_sha}_{logical}"
        dst = fanout_path(physical)
        md5_cache = dst.with_name(dst.name + ".md5")
        if dst.is_file() and dst.stat().st_size > 100_000_000:
            if not md5_cache.is_file():
                h_md5 = hashlib.md5()
                sz = 0
                with open(dst, "rb") as f:
                    while True:
                        chunk = f.read(8 * 1024 * 1024)
                        if not chunk:
                            break
                        h_md5.update(chunk)
                        sz += len(chunk)
                write_fsync(md5_cache, f"{h_md5.hexdigest()} {sz}\n".encode("utf-8"))
            results.append({"logical": logical, "physical": physical, "path": str(dst), "size": dst.stat().st_size, "status": "present"})
            continue
        dst.parent.mkdir(parents=True, exist_ok=True)
        tmp_path = dst.parent / f".{physical}.part"
        subprocess.run(
            ["curl", "-fsSL", "--retry", "5", "--retry-all-errors", "--retry-delay", "2", "-o", str(tmp_path), url],
            check=True,
        )
        with open(tmp_path, "r+b") as f:
            f.flush()
            os.fsync(f.fileno())
        h = hashlib.sha256()
        h_md5 = hashlib.md5()
        sz = 0
        with open(tmp_path, "rb") as f:
            while True:
                chunk = f.read(8 * 1024 * 1024)
                if not chunk:
                    break
                h.update(chunk)
                h_md5.update(chunk)
                sz += len(chunk)
        got_sha = h.hexdigest()
        if got_sha != expected_sha:
            tmp_path.unlink(missing_ok=True)
            raise RuntimeError(f"SHA256 mismatch for {logical}: expected {expected_sha}, got {got_sha}")
        os.chmod(tmp_path, 0o664)
        tmp_path.replace(dst)
        write_fsync(md5_cache, f"{h_md5.hexdigest()} {sz}\n".encode("utf-8"))
        results.append({"logical": logical, "physical": physical, "path": str(dst), "size": sz, "status": "downloaded"})
    return results


def ensure_qwen_campaigns() -> dict:
    """Stage and submit Qwen workunits across all GPU and CPU plan classes."""
    tmpl_dir = PROJ / "templates"
    tmpl_dir.mkdir(parents=True, exist_ok=True)
    write_fsync(tmpl_dir / "usnm_qwen_in", TEMPLATE_QWEN_IN.encode("utf-8"))
    write_fsync(tmpl_dir / "usnm_qwen_8b_in", TEMPLATE_QWEN_8B_IN.encode("utf-8"))
    write_fsync(tmpl_dir / "usnm_qwen_2b_in", TEMPLATE_QWEN_2B_IN.encode("utf-8"))
    write_fsync(tmpl_dir / "usnm_qwen_out", TEMPLATE_QWEN_OUT.encode("utf-8"))

    exec_sql(
        """
        UPDATE workunit wu
        JOIN app a ON wu.appid = a.id
        SET wu.batch = 12
        WHERE a.name = 'usnm_qwen' AND wu.batch = 0
        """
    )

    campaigns_spec = [
        {
            "name": "v1-gpu-container-8b",
            "slice": (0, 50),
            "engine_image": "sha256:d38ad661323c7c7928eb06c7398582f7ee63b32dc0f252fda2f7f990f0301b53",
            "engine_llama": "sha256:b0baccbe2e00b5d74e63f85f9f023c93d61d0f329475269e1e7002e87eb875ea",
            "engine_weights": "sha256:969266d1c24656977e06a6efacae04deac6d5093c77227fe55ac28975035cddd",
            "wu_template": "templates/usnm_qwen_in",
            "batch": "18",
            "rsc_disk_bound": "51539607552",
            "rsc_memory_bound": "6442450944",
            "rsc_fpops_est": "1e13",
            "extra_files": [],
        },
        {
            "name": "v1-gpu-native-8b",
            "slice": (50, 100),
            "engine_image": "sha256:d38ad661323c7c7928eb06c7398582f7ee63b32dc0f252fda2f7f990f0301b53",
            "engine_llama": "sha256:89f646fd0e3afb554c20297c2921b32d7900057087be8e5d8fd75720fedf4e0e",
            "engine_weights": "sha256:969266d1c24656977e06a6efacae04deac6d5093c77227fe55ac28975035cddd",
            "wu_template": "templates/usnm_qwen_8b_in",
            "batch": "8",
            "rsc_disk_bound": "19327352832",
            "rsc_memory_bound": "6442450944",
            "rsc_fpops_est": "1e13",
            "extra_files": [
                "usnm_qwen_67d1659bfe71b89d50b45a4ad1a9e5b997e5bb16ce5da66a6a6167abd569e9e2_Qwen3VL-8B-Instruct-Q4_K_M.gguf",
                "usnm_qwen_c6ba85508d82f42590e6eb77d5340369ab6fecf107a7561d809523d8aa5f3bfd_mmproj-Qwen3VL-8B-Instruct-Q8_0.gguf",
            ],
        },
        {
            "name": "v1-gpu-native-2b",
            "slice": (100, 130),
            "engine_image": "sha256:d38ad661323c7c7928eb06c7398582f7ee63b32dc0f252fda2f7f990f0301b53",
            "engine_llama": "sha256:89f646fd0e3afb554c20297c2921b32d7900057087be8e5d8fd75720fedf4e0e",
            "engine_weights": "sha256:969266d1c24656977e06a6efacae04deac6d5093c77227fe55ac28975035cddd",
            "wu_template": "templates/usnm_qwen_2b_in",
            "batch": "2",
            "rsc_disk_bound": "12884901888",
            "rsc_memory_bound": "4294967296",
            "rsc_fpops_est": "5e12",
            "extra_files": [
                "usnm_qwen_b7802e29f71a9e5b5e3f83f613df898a2204342dcea71a231ea501d481813c39_Qwen3-VL-2B-Instruct-Q8_0.gguf",
                "usnm_qwen_69066c8f279ec85ff48ab4059f6ebba0d2932ca57667f2bbdac7d9805bca9e7b_mmproj-Qwen3-VL-2B-Instruct-Q8_0.gguf",
            ],
        },
        {
            "name": "v1-cpu-container-2b",
            "slice": (130, 160),
            "engine_image": "sha256:d38ad661323c7c7928eb06c7398582f7ee63b32dc0f252fda2f7f990f0301b53",
            "engine_llama": "sha256:b0baccbe2e00b5d74e63f85f9f023c93d61d0f329475269e1e7002e87eb875ea",
            "engine_weights": "sha256:969266d1c24656977e06a6efacae04deac6d5093c77227fe55ac28975035cddd",
            "wu_template": "templates/usnm_qwen_in",
            "batch": "12",
            "rsc_disk_bound": "45097156608",
            "rsc_memory_bound": "6442450944",
            "rsc_fpops_est": "1e13",
            "extra_files": [],
        },
    ]

    all_pages: list[tuple[str, Path, Path, Path]] = []
    summary = {}
    for spec in campaigns_spec:
        cname = spec["name"]
        croot = Path("/project/qwen") / cname
        cunits = croot / "units"
        cunits.mkdir(parents=True, exist_ok=True)

        start_idx, end_idx = spec["slice"]
        want_count = end_idx - start_idx
        existing_submitted = list(cunits.glob("*/submitted"))
        existing_staged = list(cunits.glob("*/qwen.json"))
        if len(existing_submitted) >= want_count - 2:
            summary[cname] = {"staged": len(existing_staged), "submitted": len(existing_submitted), "status": "already_complete"}
            continue

        if len(existing_staged) < want_count:
            if not all_pages:
                wu_rows = q(
                    """
                    SELECT wu.id, wu.name
                    FROM workunit wu
                    JOIN app a ON wu.appid = a.id
                    WHERE a.name = 'usnm_quality'
                      AND wu.id IN (SELECT workunitid FROM result WHERE server_state = 2)
                    ORDER BY wu.id ASC
                    LIMIT 260
                    """
                )
                for row in wu_rows:
                    wname = str(row.get("name", ""))
                    if not wname.startswith("usnm_quality_"):
                        continue
                    jid = wname.removeprefix("usnm_quality_")
                    src_qjson = fanout_path(f"{jid}_quality_json")
                    src_jpg = fanout_path(f"{jid}_page_jpg")
                    src_xml = fanout_path(f"{jid}_loc_xml")
                    if not (src_qjson.is_file() and src_jpg.is_file() and src_xml.is_file()):
                        continue
                    try:
                        qjob = json.loads(src_qjson.read_text())
                        if qjob.get("pages"):
                            continue
                        p_bytes = qjob["inputs"]["page"]["bytes"]
                        a_bytes = qjob["inputs"]["alto"]["bytes"]
                        doc_id = str(qjob.get("doc_id") or jid).replace("/", "_")
                    except Exception:
                        continue
                    if src_jpg.stat().st_size == p_bytes and src_xml.stat().st_size == a_bytes:
                        all_pages.append((doc_id, src_qjson, src_jpg, src_xml))
                    if len(all_pages) >= 200:
                        break

            tmp_qroot = Path(f"/tmp/qroot-{cname}")
            tmp_qunits = tmp_qroot / "units"
            if tmp_qroot.exists():
                shutil.rmtree(tmp_qroot)
            tmp_qunits.mkdir(parents=True, exist_ok=True)
            for doc_id, src_qjson, src_jpg, src_xml in all_pages[start_idx:end_idx]:
                udir = tmp_qunits / doc_id
                udir.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(src_qjson, udir / "quality.json")
                shutil.copyfile(src_jpg, udir / "page.jpg")
                shutil.copyfile(src_xml, udir / "loc.xml")

            try:
                subprocess.run(
                    [
                        "/usr/local/bin/usnm-boinc",
                        "qwen",
                        "stage",
                        "--quality-root",
                        str(tmp_qroot),
                        "--root",
                        str(croot),
                        "--engine-image",
                        spec["engine_image"],
                        "--engine-llama",
                        spec["engine_llama"],
                        "--engine-weights",
                        spec["engine_weights"],
                        "--max-pages",
                        str(want_count),
                    ],
                    check=True,
                )
            finally:
                shutil.rmtree(tmp_qroot, ignore_errors=True)

        submitted_now = 0
        for udir in sorted(cunits.iterdir()):
            qjson_path = udir / "qwen.json"
            page_path = udir / "page.jpg"
            sub_marker = udir / "submitted"
            if not qjson_path.is_file() or sub_marker.is_file():
                continue
            job = json.loads(qjson_path.read_text())
            jid = job["id"]
            wu_name = f"usnm_qwen_{jid}"

            p_jpg_name = f"{jid}_page_jpg"
            p_json_name = f"{jid}_qwen_json"
            copy_fsync(page_path, fanout_path(p_jpg_name))
            copy_fsync(qjson_path, fanout_path(p_json_name))

            cw_cmd = [
                str(PROJ / "bin/create_work"),
                "--appname",
                "usnm_qwen",
                "--wu_name",
                wu_name,
                "--wu_template",
                spec["wu_template"],
                "--result_template",
                "templates/usnm_qwen_out",
                "--batch",
                spec["batch"],
                "--min_quorum",
                "2",
                "--target_nresults",
                "2",
                "--max_error_results",
                "3",
                "--max_total_results",
                "4",
                "--max_success_results",
                "3",
                "--rsc_fpops_est",
                spec["rsc_fpops_est"],
                "--rsc_fpops_bound",
                "1e16",
                "--rsc_memory_bound",
                spec["rsc_memory_bound"],
                "--rsc_disk_bound",
                spec["rsc_disk_bound"],
                "--delay_bound",
                "259200",
                p_jpg_name,
                p_json_name,
                *spec["extra_files"],
            ]
            res = subprocess.run(cw_cmd, cwd=str(PROJ), capture_output=True, text=True)
            if res.returncode == 0 or "Duplicate entry" in (res.stderr or ""):
                write_fsync(sub_marker, (wu_name + "\n").encode("utf-8"))
                submitted_now += 1
            else:
                raise RuntimeError(f"create_work failed for {wu_name}: {res.stderr}")

        summary[cname] = {
            "staged": len(list(cunits.glob("*/qwen.json"))),
            "submitted_now": submitted_now,
            "total_submitted": len(list(cunits.glob("*/submitted"))),
        }
    return summary


def main() -> None:
    cfg_fix = ensure_server_config_xml()

    hav_before = q(
        """
        SELECT hav.host_id, h.domain_name, hav.app_version_id, hav.max_jobs_per_day, hav.n_jobs_today, hav.consecutive_valid
        FROM host_app_version hav
        LEFT JOIN host h ON hav.host_id = h.id
        ORDER BY hav.host_id, hav.app_version_id
        """
    )
    exec_sql("UPDATE host_app_version SET max_jobs_per_day = GREATEST(max_jobs_per_day, 10000), n_jobs_today = 0")
    exec_sql("UPDATE host SET error_rate = 0 WHERE error_rate > 0")

    cifs_repair = repair_cifs_nbytes_in_mysql()
    gguf_status = []
    qwen_submit_summary = {}
    if os.environ.get("PROVISION_QWEN", "1") == "1":
        gguf_status = ensure_sticky_gguf_models()
        qwen_submit_summary = ensure_qwen_campaigns()

    ensure_server_config_xml()
    exec_sql(
        """
        UPDATE workunit wu
        JOIN app a ON wu.appid = a.id
        SET wu.priority = 100
        WHERE a.name = 'usnm_qwen' AND wu.priority < 100
        """
    )
    exec_sql(
        """
        UPDATE result r
        JOIN app a ON r.appid = a.id
        SET r.priority = 100
        WHERE a.name = 'usnm_qwen' AND r.server_state = 2 AND r.priority < 100
        """
    )
    for cache_file in PROJ.glob("html/cache/*/server_status.php*"):
        try:
            cache_file.unlink()
        except Exception:
            pass

    host_rpc = q(
        """
        SELECT id, domain_name, os_name, p_ncpus, p_ngpus, serialnum, p_model,
               FROM_UNIXTIME(rpc_time) AS last_rpc_utc, rpc_seqno, error_rate
        FROM host
        ORDER BY rpc_time DESC
        """
    )
    queue_by_batch = q(
        """
        SELECT a.name AS app_name, wu.batch, r.server_state, r.outcome, COUNT(*) AS cnt
        FROM result r
        JOIN workunit wu ON r.workunitid = wu.id
        JOIN app a ON r.appid = a.id
        GROUP BY a.name, wu.batch, r.server_state, r.outcome
        ORDER BY a.name, wu.batch, r.server_state, r.outcome
        """
    )
    app_versions = q(
        """
        SELECT av.id, a.name AS app_name, p.name AS platform, av.version_num, av.plan_class, av.deprecated
        FROM app_version av
        JOIN app a ON av.appid = a.id
        JOIN platform p ON av.platformid = p.id
        ORDER BY a.name, p.name, av.version_num
        """
    )
    recent_results = q(
        """
        SELECT r.id, r.name, a.name AS app_name, wu.batch, r.server_state, r.outcome, r.exit_status,
               r.validate_state, r.hostid, h.domain_name, av.plan_class, r.elapsed_time, r.cpu_time,
               LEFT(REPLACE(REPLACE(r.stderr_out, '\\n', ' | '), '\\t', ' '), 600) AS stderr_snip
        FROM result r
        JOIN app a ON r.appid = a.id
        JOIN workunit wu ON r.workunitid = wu.id
        LEFT JOIN host h ON r.hostid = h.id
        LEFT JOIN app_version av ON r.app_version_id = av.id
        WHERE a.name IN ('usnm_qwen', 'usnm_surya', 'usnm_qwen_train')
           OR av.plan_class LIKE '%gpu%' OR av.plan_class LIKE '%cuda%' OR av.plan_class LIKE '%apple%'
        ORDER BY r.id DESC
        LIMIT 40
        """
    )
    qwen_errors = q(
        """
        SELECT r.id, r.name, wu.batch, r.server_state, r.outcome, r.exit_status,
               r.hostid, h.domain_name, av.plan_class, r.elapsed_time,
               LEFT(REPLACE(REPLACE(r.stderr_out, '\\n', ' | '), '\\t', ' '), 1200) AS stderr_snip
        FROM result r
        JOIN app a ON r.appid = a.id
        JOIN workunit wu ON r.workunitid = wu.id
        LEFT JOIN host h ON r.hostid = h.id
        LEFT JOIN app_version av ON r.app_version_id = av.id
        WHERE a.name = 'usnm_qwen' AND r.server_state = 5
        ORDER BY r.id DESC
        LIMIT 25
        """
    )
    qual_errors = q(
        """
        SELECT r.id, r.name, r.outcome, r.exit_status, r.hostid, h.domain_name, av.plan_class, r.elapsed_time,
               LEFT(REPLACE(REPLACE(r.stderr_out, '\\n', ' | '), '\\t', ' '), 800) AS stderr_snip
        FROM result r
        JOIN app a ON r.appid = a.id
        LEFT JOIN host h ON r.hostid = h.id
        LEFT JOIN app_version av ON r.app_version_id = av.id
        WHERE a.name = 'usnm_quality' AND r.outcome = 3
        ORDER BY r.id DESC
        LIMIT 15
        """
    )

    qwen_campaigns = {}
    qroot = Path("/project/qwen")
    if qroot.is_dir():
        for d in sorted(qroot.iterdir()):
            if d.is_dir():
                u = d / "units"
                qwen_campaigns[d.name] = {
                    "staged_units": len(list(u.glob("*/qwen.json"))) if u.is_dir() else 0,
                    "submitted_markers": len(list(u.glob("*/submitted"))) if u.is_dir() else 0,
                }

    sched_lines = []
    for lp in sorted(PROJ.glob("log_*/scheduler.log")):
        try:
            if lp.is_file():
                sched_lines.extend(lp.read_text(errors="replace").splitlines()[-40:])
        except OSError:
            pass

    out = {
        "timestamp_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "config_xml_fix": cfg_fix,
        "hav_before_reset": hav_before,
        "cifs_repair": cifs_repair,
        "gguf_files": gguf_status,
        "qwen_submit_summary": qwen_submit_summary,
        "host_rpc": host_rpc,
        "queue_by_batch": queue_by_batch,
        "app_versions": app_versions,
        "recent_results": recent_results,
        "qwen_errors": qwen_errors,
        "qual_errors": qual_errors,
        "qwen_campaigns": qwen_campaigns,
        "scheduler_log_tail": sched_lines[-40:],
    }

    payload = json.dumps(out, indent=2).encode("utf-8")
    local_out_dir = Path("/tmp/qstatus_out")
    local_out_dir.mkdir(parents=True, exist_ok=True)
    (local_out_dir / "gpu-diag.json").write_bytes(payload)

    upload_url = os.environ.get("DIAG_UPLOAD_URL", "").strip()
    if upload_url:
        req = urllib.request.Request(
            upload_url,
            data=payload,
            headers={"x-ms-blob-type": "BlockBlob", "Content-Type": "application/json"},
            method="PUT",
        )
        urllib.request.urlopen(req, timeout=30).read()
    else:
        handoff_url = os.environ.get("HANDOFF_URL", "https://stusnmoffapzy.blob.core.windows.net/boinc-handoff")
        subprocess.run(
            [
                "usnm-boinc",
                "handoff",
                "put",
                "--from",
                str(local_out_dir),
                "--to-url",
                handoff_url,
                "--prefix",
                "quality/status",
            ],
            check=True,
        )


if __name__ == "__main__":
    main()
