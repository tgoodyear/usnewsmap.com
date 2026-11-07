#!/usr/bin/env python3
"""Run a native OCR unit under a stock BOINC client, against a small local stand-in project (#1418).

The stand-in speaks just enough of the scheduler and data-server protocols for one client on
this host: nothing reaches the real project, no host is involved, and nothing is signed, because
the client runs the build as an anonymous-platform app (`app_info.xml`). Scenarios:

- plain: the task runs to the end and its files come back.
- suspend: the task is suspended and resumed while Tesseract runs.
- restart: the client is stopped mid-bundle and started again. The task resumes from its
  checkpoint and doesn't read the finished pages twice.
- wrong-build: the unit names another build, and the task fails without reading a page.
"""
import argparse
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import re
import shutil
import subprocess
import threading
import time

APP = "usnm_ocr_native"
VERSION = 100
AUTHENTICATOR = "0123456789abcdef0123456789abcdef"
PORT = 8123
# Not the default, so a client already on this host is never the one spoken to.
RPC_PORT = 31499
URL = f"http://127.0.0.1:{PORT}/"


def md5(path):
    return hashlib.md5(Path(path).read_bytes()).hexdigest()


class Project:
    """What the scheduler sends, once, and what comes back."""

    def __init__(self, slot, inputs, outputs, out):
        self.files = {f"in_{name}": slot / name for name in inputs}
        self.inputs, self.outputs, self.out = inputs, outputs, out
        self.sent = False
        self.reported = threading.Event()
        self.exit_status = None
        self.stderr = ""

    def reply(self, request):
        for match in re.finditer(r"<result>(.*?)</result>", request, re.S):
            status = re.search(r"<exit_status>(-?\d+)</exit_status>", match[1])
            if status and "<name>wu_0</name>" in match[1]:
                self.exit_status = int(status[1])
                self.stderr = match[1]
                self.reported.set()
        xml = [f"<scheduler_reply>\n<scheduler_version>800</scheduler_version>\n<master_url>{URL}</master_url>\n"
               "<project_name>native ocr test</project_name>\n<request_delay>1</request_delay>\n"
               "<userid>1</userid>\n<user_name>test</user_name>\n<hostid>1</hostid>\n"
               "<host_create_time>1700000000</host_create_time>\n"]
        if "<result>" in request:
            xml.append("<result_ack><name>wu_0</name></result_ack>\n")
        if not self.sent and "<work_req_seconds>0" not in request:
            self.sent = True
            xml.append(self.work())
        xml.append("</scheduler_reply>\n")
        return "".join(xml)

    def work(self):
        xml = [f"<app><name>{APP}</name><user_friendly_name>native ocr</user_friendly_name></app>\n"]
        for name, path in self.files.items():
            xml.append(f"<file_info><name>{name}</name><url>{URL}dl/{name}</url>"
                       f"<nbytes>{path.stat().st_size}</nbytes><md5_cksum>{md5(path)}</md5_cksum></file_info>\n")
        for n in range(len(self.outputs)):
            xml.append(f"<file_info><name>wu_0_r{n}</name><generated_locally/><upload_when_present/>"
                       f"<max_nbytes>1073741824</max_nbytes><url>{URL}file_upload_handler</url></file_info>\n")
        refs = "".join(f"<file_ref><file_name>in_{name}</file_name><open_name>{name}</open_name><copy_file/></file_ref>\n"
                       for name in self.inputs)
        xml.append(f"<workunit><name>wu</name><app_name>{APP}</app_name><version_num>{VERSION}</version_num>"
                   "<rsc_fpops_est>1e12</rsc_fpops_est><rsc_fpops_bound>1e16</rsc_fpops_bound>"
                   "<rsc_memory_bound>1610612736</rsc_memory_bound><rsc_disk_bound>4e9</rsc_disk_bound>\n"
                   f"{refs}</workunit>\n")
        refs = "".join(f"<file_ref><file_name>wu_0_r{n}</file_name><open_name>{name}</open_name><copy_file/></file_ref>\n"
                       for n, name in enumerate(self.outputs))
        xml.append(f"<result><name>wu_0</name><wu_name>wu</wu_name><version_num>{VERSION}</version_num>"
                   f"<report_deadline>{int(time.time()) + 86400}</report_deadline>\n{refs}</result>\n")
        return "".join(xml)


def handler(project):
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def send(self, body, kind="text/xml"):
            self.send_response(200)
            self.send_header("Content-Type", kind)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            if self.path.startswith("/dl/"):
                self.send(project.files[self.path[4:]].read_bytes(), "application/octet-stream")
            else:
                self.send(f"<html><head><!-- <scheduler>{URL}sched</scheduler> --></head></html>".encode(),
                          "text/html")

        def do_POST(self):
            body = self.rfile.read(int(self.headers["Content-Length"]))
            if self.path == "/sched":
                self.send(project.reply(body.decode("utf-8", "replace")).encode())
                return
            if b"<get_file_size>" in body:
                self.send(b"<data_server_reply><status>0</status><file_size>0</file_size></data_server_reply>\n")
                return
            head, _, data = body.partition(b"<data>\n")
            name = head.partition(b"<name>")[2].partition(b"</name>")[0].decode()
            (project.out / name).write_bytes(data)
            self.send(b"<data_server_reply><status>0</status></data_server_reply>\n")

    return Handler


def app_info(build):
    files = sorted(p.name for p in build.iterdir())
    infos = "".join(f"<file_info><name>{n}</name>{'<executable/>' if n in ('usnm_ocr', 'usnm_tesseract') else ''}</file_info>\n"
                    for n in files)
    refs = "".join(f"<file_ref><file_name>{n}</file_name><open_name>{n}</open_name></file_ref>\n"
                   for n in files if n != "usnm_ocr")
    return (f"<app_info>\n<app><name>{APP}</name><user_friendly_name>native ocr</user_friendly_name></app>\n{infos}"
            f"<app_version><app_name>{APP}</app_name><version_num>{VERSION}</version_num><api_version>8.3.0</api_version>\n"
            f"<file_ref><file_name>usnm_ocr</file_name><main_program/></file_ref>\n{refs}</app_version>\n</app_info>\n")


class Client:
    def __init__(self, a):
        self.a = a
        self.data = a.work / "client"
        self.log = open(a.work / "client.log", "ab")

    def start(self, *args):
        self.process = subprocess.Popen(
            [str(self.a.boinc), "--dir", str(self.data), "--allow_multiple_clients",
             "--no_gpus", "--no_priority_change", "--gui_rpc_port", str(RPC_PORT), *args], stdout=self.log, stderr=self.log)

    def cmd(self, *args):
        return subprocess.run([str(self.a.boinccmd), "--host", f"localhost:{RPC_PORT}", "--passwd", "test", *args],
                              cwd=self.data, capture_output=True, text=True)

    def task(self):
        tasks = self.cmd("--get_tasks").stdout
        match = re.search(r"active_task_state: (\S+)", tasks)
        fraction = re.search(r"fraction done: ([0-9.]+)", tasks)
        return (match[1] if match else None, float(fraction[1]) if fraction else 0.0)

    def stop(self):
        self.cmd("--quit")
        self.process.wait(timeout=60)


def wait(predicate, seconds, what):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(1)
    raise TimeoutError(what)


def engine_state(work):
    """The state of each engine of this test's client: other work on the host may run Tesseract."""
    pids = subprocess.run(["pgrep", "-f", f"{work}/client/.*usnm_tesseract"],
                          capture_output=True, text=True).stdout.split()
    states = []
    for pid in pids:
        try:
            states.append(Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[0])
        except OSError:
            pass
    return states


def scenario(a, client, project):
    evidence = {"scenario": a.scenario}
    engines = lambda: engine_state(a.work)
    wait(lambda: client.task()[0] == "EXECUTING" or project.exit_status is not None, 300, "the task never started")
    if a.scenario == "suspend":
        wait(engines, 120, "no engine")
        time.sleep(5)
        client.cmd("--task", URL, "wu_0", "suspend")
        wait(lambda: engines() and all(s == "T" for s in engines()), 30, "engine not stopped")
        evidence["engine_while_suspended"] = engines()
        time.sleep(30)
        client.cmd("--task", URL, "wu_0", "resume")
        wait(lambda: engines() and all(s != "T" for s in engines()), 30, "engine not resumed")
        evidence["engine_after_resume"] = engines()
    if a.scenario == "restart":
        checkpoint = a.work / "client/slots/0/ocr_checkpoint.json"
        wait(checkpoint.exists, a.timeout_secs, "no checkpoint")
        evidence["pages_done_before_stop"] = len(json.loads(checkpoint.read_text())["pages"])
        client.stop()
        evidence["engines_after_client_stop"] = len(engines())
        time.sleep(5)
        client.start()
        wait(engines, 120, "the task did not restart")
    deadline = time.monotonic() + a.timeout_secs
    while not project.reported.wait(6) and time.monotonic() < deadline:
        client.cmd("--project", URL, "update")
    evidence["exit_status"] = project.exit_status
    evidence["uploaded"] = {f.name: f.stat().st_size for f in sorted((a.work / "out").iterdir())}
    # The client sends the task's stderr with its report.
    evidence["resumed_from_checkpoint"] = "resumed from checkpoint" in project.stderr
    evidence["refused_as_another_build"] = "made for build" in project.stderr
    return evidence


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--boinc", type=Path, required=True)
    p.add_argument("--boinccmd", type=Path, required=True)
    p.add_argument("--build", type=Path, required=True)
    p.add_argument("--fixture", type=Path, required=True)
    p.add_argument("--scan", type=Path, action="append", required=True)
    p.add_argument("--language", required=True)
    p.add_argument("--scenario", choices=("plain", "suspend", "restart", "wrong-build"), required=True)
    p.add_argument("--work", type=Path, required=True)
    p.add_argument("--timeout-secs", type=int, default=3600)
    a = p.parse_args()
    if a.work.exists():
        shutil.rmtree(a.work)
    (a.work / "out").mkdir(parents=True)
    unit = a.work / "unit"
    image = "sha256:" + hashlib.sha256((a.build / "usnm_native_build.json").read_bytes()).hexdigest()
    if a.scenario == "wrong-build":
        image = "sha256:" + "2" * 64
    subprocess.run([str(a.fixture), *(x for s in a.scan for x in ("--scan", str(s))), "--language", a.language,
                    "--image", image, "--dir", str(unit)], check=True)
    if len(a.scan) > 1:
        inputs, outputs = ["ocr_bundle.json", "pages"], ["results.json", "texts", "layouts"]
    else:
        inputs, outputs = ["page", "request.json"], ["text.txt", "layout.xml", "result.json"]
    project = Project(unit, inputs, outputs, a.work / "out")
    server = ThreadingHTTPServer(("127.0.0.1", PORT), handler(project))
    threading.Thread(target=server.serve_forever, daemon=True).start()

    client = Client(a)
    directory = client.data / "projects" / f"127.0.0.1_{PORT}"
    directory.mkdir(parents=True)
    (client.data / "gui_rpc_auth.cfg").write_text("test\n")
    for f in a.build.iterdir():
        shutil.copyfile(f, directory / f.name)
    (directory / "usnm_ocr").chmod(0o755)
    (directory / "usnm_tesseract").chmod(0o755)
    (directory / "app_info.xml").write_text(app_info(a.build))
    (client.data / "cc_config.xml").write_text("<cc_config><log_flags><task_debug>1</task_debug></log_flags>"
                                                "<options><allow_remote_gui_rpc>0</allow_remote_gui_rpc></options></cc_config>\n")
    (client.data / "global_prefs_override.xml").write_text(
        "<global_preferences><run_if_user_active>1</run_if_user_active><max_ncpus_pct>25</max_ncpus_pct>"
        "<cpu_usage_limit>100</cpu_usage_limit><ram_max_used_busy_pct>90</ram_max_used_busy_pct>"
        "<ram_max_used_idle_pct>90</ram_max_used_idle_pct><work_buf_min_days>0.01</work_buf_min_days>"
        "<suspend_cpu_usage>0</suspend_cpu_usage></global_preferences>\n")
    client.start("--attach_project", URL, AUTHENTICATOR)
    try:
        evidence = scenario(a, client, project)
    finally:
        client.cmd("--quit")
        try:
            client.process.wait(timeout=60)
        except subprocess.TimeoutExpired:
            client.process.kill()
    print(json.dumps(evidence, indent=1))
    (a.work / "evidence.json").write_text(json.dumps(evidence, indent=1))
    wrong = a.scenario == "wrong-build"
    passed = (evidence["exit_status"] != 0 and evidence["refused_as_another_build"]) if wrong else (
        evidence["exit_status"] == 0 and len(evidence["uploaded"]) == len(outputs)
        and (a.scenario != "restart" or evidence["resumed_from_checkpoint"]))
    if not passed:
        raise SystemExit(f"the {a.scenario} scenario failed")


if __name__ == "__main__":
    main()
