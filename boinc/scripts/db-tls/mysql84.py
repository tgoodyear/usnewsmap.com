"""Actual pinned BOINC consumers against a separate private MySQL 8.4 server."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import time

import fixture

# The client's only writable place.
SHARE = fixture.CA.parent
POLICY = "/checks/qwen-tiebreak-policy.py"


def observe():
    version, required = fixture.run(
        *fixture.command("cli", "tls-db.test")[:-2], "-e",
        "SELECT VERSION(), @@require_secure_transport").stdout.strip().split("\t")
    if not version.startswith("8.4.") or required != "1":
        raise RuntimeError("expected MySQL 8.4 with require_secure_transport=ON")
    print(json.dumps({"server": version, "require_secure_transport": True,
                      "patch_sha256": hashlib.sha256(
                          Path("/checks/patch-server-db-tls.py").read_bytes()).hexdigest()}),
          flush=True)


def plaintext_rejected(result):
    return result.returncode != 0 and result.stderr.startswith("ERROR 3159 (HY000): ")


def cli():
    return fixture.command("cli", "tls-db.test")[:-2]


def sql(query):
    result = subprocess.run([*cli(), "-e", query], capture_output=True, text=True, timeout=60)
    if result.returncode:
        raise RuntimeError(f"fixture SQL failed: {result.stderr.strip()[-300:]}")
    return result.stdout.strip()


def mark_as_sent(result, now):
    """The scheduler's dispatch of one replica: BOINC's `DB_RESULT::mark_as_sent`."""
    return (f"update result set server_state=4, hostid=1, userid=1, sent_time={now}, "
            f"report_deadline={now + 86400}, flops_estimate=1.000000000000000e+09, app_version_id=0  "
            f"where id={result} and server_state=2")


def qwen_tiebreak():
    """#1477: settle disputed Qwen units on MySQL 8.4 while one unit's row is locked
    and the scheduler dispatches another unit's unsent replica."""
    for name in ("schema.sql", "constraints.sql"):
        with open(f"/opt/boinc/db/{name}", encoding="utf-8") as f:
            subprocess.run(cli(), stdin=f, check=True, capture_output=True, text=True, timeout=120)
    now = int(sql("SELECT UNIX_TIMESTAMP()"))
    app = fixture.insert_row(sql, "fixture", "app", create_time=now, name="usnm_qwen", user_friendly_name="Qwen")
    units, unsent = {}, {}
    for case in ("settle", "locked", "dispatch"):
        name = f"usnm_qwen_tiebreak-{case}"
        units[case] = fixture.insert_row(
            sql, "fixture", "workunit", create_time=now - 60, appid=app, name=name, batch=8, min_quorum=2,
            target_nresults=3, max_error_results=3, max_total_results=4, max_success_results=4,
            delay_bound=86400, transition_time=now + 86400, rsc_fpops_est=1e12, rsc_fpops_bound=1e13,
            size_class=-1)
        # The first pair disagreed; the tie-breaker is unsent.
        for i, (state, outcome, validate) in enumerate(((5, 1, 4), (5, 1, 4), (2, 0, 0))):
            unsent[case] = fixture.insert_row(
                sql, "fixture", "result", create_time=now - 60, workunitid=units[case], appid=app, batch=8,
                name=f"{name}_{i}", server_state=state, outcome=outcome, validate_state=validate,
                report_deadline=now + 86400, elapsed_time=300.0, random=i, size_class=-1)
        job = SHARE / f"qwen/campaign/units/tiebreak-{case}"
        job.mkdir(parents=True)
        (job / "qwen.json").write_text(json.dumps(
            {"id": f"tiebreak-{case}", "language": "yid", "prompt_version": 1}))
    # Statistics as on a populated server, so lookups by unit take BOINC's indexes.
    sql("ANALYZE TABLE workunit, result")
    config = SHARE / "config.xml"
    config.write_text("<boinc><config><db_host>tls-db.test</db_host><db_user>fixture</db_user>"
                      "<db_passwd>fixture-only</db_passwd><db_name>fixture</db_name></config></boinc>")
    journal = SHARE / "qwen-tiebreak-journal.jsonl"
    since, until = (time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(t)) for t in (now - 3600, now + 3600))
    apply = ["python3", POLICY, "settle", "--apply", "--max-units", "5", "--journal", str(journal),
             "--since", since, "--until", until, "--stop-tiebreakers", "yid:1", "--config", str(config),
             "--qwen-root", str(SHARE / "qwen"), "--download", str(SHARE / "download"),
             "--db-ca", str(fixture.CA), "--db-defaults", str(fixture.OPTIONS)]

    def state(wu):
        return (sql("SELECT CONCAT_WS(',',error_mask,max_total_results,canonical_resultid) "
                    f"FROM workunit WHERE id={wu}"),
                sql("SELECT GROUP_CONCAT(CONCAT_WS(',',server_state,outcome,validate_state) ORDER BY id "
                    f"SEPARATOR ';') FROM result WHERE workunitid={wu}"))

    def session(statements):
        # -n flushes each statement's output, so the caller can read it while the session sleeps.
        process = subprocess.Popen([*cli(), "-n", "-e", statements], stdout=subprocess.PIPE,
                                   stderr=subprocess.DEVNULL, text=True)
        return process, process.stdout.readline().split()

    # A holder keeps one unit's row locked; a scheduler session has dispatched another
    # unit's tie-breaker and not yet committed.
    holder, (locked, holder_id) = session(
        f"START TRANSACTION; SELECT id, CONNECTION_ID() FROM workunit WHERE id={units['locked']} FOR UPDATE; "
        "SELECT SLEEP(120)")
    dispatcher, (dispatched, dispatcher_id) = session(
        f"START TRANSACTION; {mark_as_sent(unsent['dispatch'], now)}; SELECT ROW_COUNT(), CONNECTION_ID(); "
        "SELECT SLEEP(120); COMMIT")
    try:
        assert locked == str(units["locked"]) and dispatched == "1", (locked, dispatched)
        settle = subprocess.Popen(apply, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        # Let the dispatch commit while the tool waits for the replica's row lock.
        waiting = ("SELECT id,server_state,outcome,validate_state FROM result "
                   f"WHERE workunitid={units['dispatch']} ORDER BY id FOR UPDATE")
        deadline = time.monotonic() + 60
        while not sql(f"SELECT id FROM information_schema.processlist WHERE info='{waiting}'"):
            assert settle.poll() is None and time.monotonic() < deadline, "the tool never waited for the dispatch"
            time.sleep(0.1)
        sql(f"KILL QUERY {int(dispatcher_id)}")
        assert dispatcher.wait(timeout=30) == 0, "the dispatch did not commit"
        out, err = settle.communicate(timeout=60)
        assert settle.returncode == 0, err[-300:]
    finally:
        # Ending the holder's session rolls its transaction back and releases the lock.
        sql(f"KILL {int(holder_id)}")
        for process in (holder, dispatcher):
            process.kill()
            process.wait(timeout=20)
    outcomes = json.loads(out)["outcomes"]
    assert outcomes == {"settled": 1, "locked": 1, "replica running": 1}, outcomes
    outcomes = json.loads(fixture.run(*apply).stdout)["outcomes"]
    assert outcomes == {"settled": 1}, outcomes
    for case in ("settle", "locked"):
        # Too many total results, the cap lowered to the three rows, and the tie-breaker OVER/DIDNT_NEED.
        assert state(units[case]) == ("8,3,0", "5,1,4;5,1,4;5,5,0"), state(units[case])
    assert state(units["dispatch"]) == ("0,4,0", "5,1,4;5,1,4;4,0,0"), state(units["dispatch"])
    # A scheduler holding the cancelled tie-breaker in its feeder cache can't send it.
    assert sql(f"{mark_as_sent(unsent['settle'], now)}; SELECT ROW_COUNT()") == "0"
    events = [json.loads(line) for line in journal.read_text().splitlines()]
    assert [e["event"] for e in events] == ["run", "begin", "settled", "skipped", "skipped", "done",
                                            "run", "begin", "settled", "done"], events
    assert [e["reason"] for e in events if e["event"] == "skipped"] == ["locked", "replica running"], events
    print(json.dumps({"case": "mysql84-qwen-tiebreak-settle", "settled": 2, "skipped_under_lock": 1,
                      "skipped_after_dispatch": 1, "cancelled_replica_dispatch_rows": 0}), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("phase", choices=("prepare", "matrix", "restart"))
    args = parser.parse_args()
    if args.phase == "prepare":
        fixture.prepare_tls()
        return
    shutil.copyfile(fixture.ROOT / "ca.pem", fixture.CA)
    fixture.OPTIONS.write_text("[client]\nssl-enforce=1\nssl-verify-server-cert=1\n"
                               f"ssl-ca={fixture.CA}\n")
    observe()
    if args.phase == "restart":
        fixture.matrix("mysql84-tls-restart", True)
        print(json.dumps({"result": "PASS", "scope": "MySQL 8.4 pinned connectors; not Azure rollout"}),
              flush=True)
        return
    fixture.matrix("mysql84-valid", True)
    fixture.matrix("mysql84-wrong-hostname", False, "wrong-db.test")
    shutil.copyfile(fixture.ROOT / "wrong-ca.pem", fixture.CA)
    fixture.matrix("mysql84-wrong-ca", False)
    shutil.copyfile(fixture.ROOT / "ca.pem", fixture.CA)
    fixture.matrix("mysql84-restored-ca", True)
    plain = subprocess.run(
        ["mariadb", "--no-defaults", "--protocol=tcp", "--host=tls-db.test", "--skip-ssl",
         "-ufixture", "-pfixture-only", "fixture", "-e", "SELECT 1"],
        capture_output=True, text=True, timeout=15)
    if not plaintext_rejected(plain):
        raise RuntimeError("plaintext control was not rejected by the server TLS policy")
    print(json.dumps({"case": "mysql84-server-requires-tls", "consumer": "unpatched-cli",
                      "outcome": "rejected", "mysql_errno": 3159}), flush=True)
    qwen_tiebreak()


if __name__ == "__main__":
    main()
