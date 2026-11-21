#!/usr/bin/env python3
"""Read-only, bounded inventory for the opt-in OCR batch controller."""

import argparse
import json
import os
import re
import stat
import subprocess
import time
from pathlib import Path


ID = re.compile(r"[0-9a-f]{64}")
STAGES = {"prepared", "reserved", "submitting", "submitted", "published", "failed"}
MAX_JOBS = 10000
MAX_FILES = 100000
# Directories the spool lookup searches: `results` and its batch directories (#2008).
MAX_SPOOL_DIRS = 1000
# Output-name lookups a run may make (directories times names): at 5 names for each of 10,000
# journals, 100 directories.
MAX_LOOKUPS = 5000000
# Workunits stay in the database for BOINC's delete_delay_hours (168) after they finish, so the
# existing-workunit count runs well ahead of the journals and needs its own, larger cap (#1483).
# USNM_MAX_EXISTING_WORKUNITS in bridge.env overrides it, within 1..MAX_EXISTING_WORKUNITS_CEILING.
DEFAULT_MAX_EXISTING_WORKUNITS = 50000
MAX_EXISTING_WORKUNITS_CEILING = 200000


def max_existing_workunits(environ=None):
    environ = os.environ if environ is None else environ
    value = environ.get("USNM_MAX_EXISTING_WORKUNITS", "").strip()
    if not value:
        return DEFAULT_MAX_EXISTING_WORKUNITS
    if not value.isdecimal() or not 1 <= int(value) <= MAX_EXISTING_WORKUNITS_CEILING:
        raise ValueError("invalid USNM_MAX_EXISTING_WORKUNITS")
    return int(value)


# Unassimilated workunits are counted once, per app, so the total and each app's share are one
# read: a submission between two counts can't make other apps' work look larger than all of it
# (#1474). Workunits of no known app have an empty name and count only in the total.
# Each active host's CPU count (`p_ncpus`, as its client last reported it) follows the count, so
# a native OCR plan can give a host one unit per CPU (#1780).
QUERY = """
SELECT COUNT(*) FROM host WHERE rpc_time > UNIX_TIMESTAMP() - 86400;
SELECT 'host', id, p_ncpus FROM host WHERE rpc_time > UNIX_TIMESTAMP() - 86400
 ORDER BY id
 LIMIT {host_limit};
SELECT 'minver', min_version FROM app WHERE name = 'usnm_ocr';
SELECT 'version', platform.name, app_version.plan_class, app_version.version_num
 FROM app_version JOIN app ON app.id = app_version.appid
 JOIN platform ON platform.id = app_version.platformid
 WHERE app.name = 'usnm_ocr' AND app_version.deprecated = 0
 ORDER BY platform.name, app_version.plan_class, app_version.version_num
 LIMIT {version_limit};
SELECT 'app', COALESCE(app.name, ''), COUNT(*) FROM workunit LEFT JOIN app ON app.id = workunit.appid
 WHERE workunit.assimilate_state != 2
 GROUP BY app.name
 ORDER BY app.name;
SELECT 'native', assignment.target_id, workunit.name FROM workunit
 JOIN app ON app.id = workunit.appid
 JOIN assignment ON assignment.workunitid = workunit.id AND assignment.target_type = 1
 WHERE app.name = 'usnm_ocr_native' AND workunit.assimilate_state != 2
 ORDER BY assignment.target_id, workunit.name
 LIMIT {native_limit};
SELECT name, assimilate_state FROM workunit
 WHERE name REGEXP '^usnm_(ocrb_)?[0-9a-f]{64}$'
 ORDER BY name
 LIMIT {limit}
"""


def query(limit=DEFAULT_MAX_EXISTING_WORKUNITS):
    """QUERY reading one existing workunit past `limit`, so an oversized inventory is seen."""
    return (QUERY.replace("{limit}", str(limit + 1)).replace("{version_limit}", str(MAX_VERSIONS + 1))
            .replace("{host_limit}", str(MAX_HOSTS + 1)).replace("{native_limit}", str(MAX_NATIVE_UNITS + 1)))


# Active hosts whose CPU count is reported, read one past so an oversized list is seen.
MAX_HOSTS = 2000
HOST_ROW = re.compile(r"host\t([1-9][0-9]{0,9})\t(-?[0-9]{1,9})")


def host_cpus(rows):
    """{host id: CPUs} from the host rows. A negative count (no report yet) is 0. Raises on
    anything malformed, a repeated host, or more than MAX_HOSTS."""
    matches = [HOST_ROW.fullmatch(row) for row in rows]
    if not all(matches) or len(matches) > MAX_HOSTS or len({m[1] for m in matches}) != len(matches):
        raise ValueError("invalid host inventory")
    return {m[1]: max(0, int(m[2])) for m in matches}


# Native OCR units not yet assimilated, by the host each is assigned to (#1780): every native
# plan's, its spot replays' and retries' alike, so a plan counts the units other plans have on a
# host against that host's room. Read one past; more than this leaves the key out, and a native
# plan that needs it then sends nothing.
MAX_NATIVE_UNITS = 20000
NATIVE_ROW = re.compile(r"native\t([1-9][0-9]{0,9})\t(usnm_[A-Za-z0-9_.-]{1,250})")
JOB_WORKUNIT = re.compile(r"usnm_([0-9a-f]{64})")


def native_host_units(rows):
    """{host id: {"units": n, "jobs": [job ids]}} from the native rows: each host's unassimilated
    usnm_ocr_native units, and the job ids of those named usnm_<job id>. None past
    MAX_NATIVE_UNITS. Raises on anything malformed or a repeated unit."""
    matches = [NATIVE_ROW.fullmatch(row) for row in rows]
    if not all(matches) or len({m[2] for m in matches}) != len(matches):
        raise ValueError("invalid native unit inventory")
    if len(matches) > MAX_NATIVE_UNITS:
        return None
    hosts = {}
    for m in matches:
        entry = hosts.setdefault(m[1], {"units": 0, "jobs": []})
        entry["units"] += 1
        job = JOB_WORKUNIT.fullmatch(m[2])
        if job:
            entry["jobs"].append(job[1])
    return hosts


# The hosts the trust list admits for native OCR (#1780), as `usnm-boinc host-trust
# native-trusted` prints them from the BOINC host's gold/trust.json: build digest -> page
# language -> host ids. Submission refuses any other host (Trust::refuse_native_job).
MAX_TRUSTED_BUILDS = 64
MAX_TRUSTED_HOSTS = 100000
DIGEST = re.compile(r"sha256:[0-9a-f]{64}")
LANGUAGE = re.compile(r"[a-z]{3}")


def native_trusted_hosts(document):
    """The checked native-trusted document. Raises on anything malformed or oversized."""
    if not isinstance(document, dict) or len(document) > MAX_TRUSTED_BUILDS:
        raise ValueError("invalid native trusted hosts")
    total = 0
    for digest, languages in document.items():
        if not DIGEST.fullmatch(digest) or not isinstance(languages, dict) or len(languages) > 64:
            raise ValueError("invalid native trusted hosts")
        for language, hosts in languages.items():
            if not LANGUAGE.fullmatch(language) or not isinstance(hosts, list) or \
                    any(type(h) is not int or not 1 <= h <= 9999999999 for h in hosts) or \
                    len(set(hosts)) != len(hosts):
                raise ValueError("invalid native trusted hosts")
            total += len(hosts)
    if total > MAX_TRUSTED_HOSTS:
        raise ValueError("invalid native trusted hosts")
    return document


# The usnm_ocr versions the feeder can load (#1687): its app's min_version and every version not
# deprecated in the database. The controller checks a bundle's pinned version against them.
MAX_VERSIONS = 200
MINVER_ROW = re.compile(r"minver\t([0-9]+)")
VERSION_ROW = re.compile(r"version\t([A-Za-z0-9_.-]{1,254})\t([A-Za-z0-9_.-]{0,254})\t([1-9][0-9]{0,8})")


def ocr_app_versions(rows):
    """{"min_version", "versions"} from the minver and version rows, or None without the app's
    row (no usnm_ocr registered). Raises on anything malformed or over the limit."""
    minver = [MINVER_ROW.fullmatch(row) for row in rows if row.startswith("minver\t")]
    versions = [VERSION_ROW.fullmatch(row) for row in rows if row.startswith("version\t")]
    if len(minver) > 1 or not all(minver) or not all(versions) or len(versions) > MAX_VERSIONS:
        raise ValueError("invalid app version inventory")
    if not minver:
        if versions:
            raise ValueError("invalid app version inventory")
        return None
    entries = [{"platform": m[1], "plan_class": m[2], "version_num": int(m[3])} for m in versions]
    if len({(e["platform"], e["plan_class"], e["version_num"]) for e in entries}) != len(entries):
        raise ValueError("invalid app version inventory")
    return {"min_version": int(minver[0][1]), "versions": entries}


SPOOL_FILE = re.compile(
    r"usnm_([0-9a-f]{64})(__file_(result\.json|text\.txt|layout\.xml)|_error|_no_output_files)")
# An OCR bundle's result (#1059): one workunit, usnm_ocrb_<bundle>, for several pages.
BUNDLE_SPOOL_FILE = re.compile(
    r"usnm_ocrb_([0-9a-f]{64})(__file_(results\.json|texts|layouts)|_error|_no_output_files)")
BUNDLE_PREFIX = "ocrb_"
SPOOL_SUFFIXES = ("__file_result.json", "__file_text.txt", "__file_layout.xml", "_error", "_no_output_files")
BUNDLE_SPOOL_SUFFIXES = ("__file_results.json", "__file_texts", "__file_layouts", "_error", "_no_output_files")
APP_NAME = r"[A-Za-z0-9_.-]{1,254}"
APP_ROW = re.compile(rf"app\t({APP_NAME})?\t([0-9]+)")
MAX_APPS = 100


def spool_candidates(results, jobs, bundles=()):
    """The regular files under `results` named as a known job's assimilator output, with their
    name match. The walk looks up only the paths the inventory needs (#2008): it lists `results`
    itself, whose entries are the batch directories (the assimilator writes one directory per
    BOINC batch, results/<batch>/, not listed here), and then for each of those directories and
    `results` it looks up each journalled job's and bundle's output names with one lstat each.
    The tens of thousands of other apps' results (usnm_quality's, mostly) are neither listed
    nor statted, so the cost follows the controller's own journals and not the size of the
    results tree (#932 made the walk a scandir, which still listed all of them). A spool
    nested deeper than results/<batch>/ isn't looked for. Symlinks are neither followed nor
    returned. Returns the matches and the number of entries visited: the entries of `results`
    and the output files found. More than MAX_FILES of them, more than MAX_SPOOL_DIRS
    directories or more than MAX_LOOKUPS lookups fail closed. The inventory reports the
    count (#1483)."""
    count = 0
    directories = [results]
    with os.scandir(results) as entries:
        for entry in entries:
            count += 1
            if count > MAX_FILES:
                raise ValueError("inventory spool limit reached; archive drained results")
            if entry.is_dir(follow_symlinks=False):
                directories.append(Path(entry.path))
    if len(directories) > MAX_SPOOL_DIRS:
        raise ValueError("inventory spool limit reached; too many results directories")
    names = [("usnm_" + job + suffix, SPOOL_FILE) for job in sorted(jobs) for suffix in SPOOL_SUFFIXES]
    names += [("usnm_" + BUNDLE_PREFIX + bundle + suffix, BUNDLE_SPOOL_FILE)
              for bundle in sorted(bundles) for suffix in BUNDLE_SPOOL_SUFFIXES]
    if len(directories) * len(names) > MAX_LOOKUPS:
        raise ValueError("inventory spool limit reached; too many lookups")
    found = []
    for directory in directories:
        for name, pattern in names:
            path = directory / name
            try:
                mode = os.lstat(path).st_mode
            except (FileNotFoundError, NotADirectoryError):
                continue
            if not stat.S_ISREG(mode):
                continue
            count += 1
            if count > MAX_FILES:
                raise ValueError("inventory spool limit reached; archive drained results")
            found.append((path, pattern.fullmatch(name)))
    return found, count


def _ids(values, message):
    if len(values) > MAX_JOBS or len(set(values)) != len(values) or \
            any(not ID.fullmatch(value) for value in values):
        raise ValueError(message)


def inventory(root, results, counts, nonce, pending_ids=(), existing_ids=None,
              pending_bundles=(), existing_bundles=None, pending_by_app=None,
              existing_limit=DEFAULT_MAX_EXISTING_WORKUNITS, app_versions=None, cpus=None,
              native_units=None, native_trusted=None):
    """pending_ids/existing_ids are single-page workunits' job ids; pending_bundles and
    existing_bundles are OCR bundle workunits' bundle ids (#1059), reported as their pages,
    which the server's journals name (record["bundle"]). pending_by_app is each app's
    unassimilated workunits, so the controller can tell other apps' work from OCR's (#1474)."""
    if not re.fullmatch(r"[0-9a-f]{32}", nonce):
        raise ValueError("invalid inventory nonce")
    if len(counts) != 2 or any(type(n) is not int or n < 0 for n in counts):
        raise ValueError("invalid inventory counts")
    if pending_by_app is not None and (
            not isinstance(pending_by_app, dict) or len(pending_by_app) > MAX_APPS or
            any(not isinstance(name, str) or not re.fullmatch(APP_NAME, name) or
                type(n) is not int or n < 0 for name, n in pending_by_app.items())):
        raise ValueError("invalid per-app inventory counts")
    if len(pending_ids) > MAX_JOBS or len(set(pending_ids)) != len(pending_ids) or \
            any(not ID.fullmatch(job_id) for job_id in pending_ids):
        raise ValueError("invalid/oversized pending job inventory")
    existing_ids = pending_ids if existing_ids is None else existing_ids
    existing_bundles = pending_bundles if existing_bundles is None else existing_bundles
    _ids(pending_bundles, "invalid/oversized pending bundle inventory")
    _ids(existing_bundles, "invalid/oversized existing bundle inventory")
    if not set(pending_bundles) <= set(existing_bundles):
        raise ValueError("invalid/oversized existing bundle inventory")
    if type(existing_limit) is not int or not 1 <= existing_limit <= MAX_EXISTING_WORKUNITS_CEILING:
        raise ValueError("invalid existing workunit limit")
    if len(existing_ids) > existing_limit or len(set(existing_ids)) != len(existing_ids) or \
            any(not ID.fullmatch(job_id) for job_id in existing_ids) or \
            not set(pending_ids) <= set(existing_ids):
        raise ValueError("invalid/oversized existing workunit inventory")
    if not root.is_dir() or not results.is_dir() or (root / "jobs").is_symlink():
        raise ValueError("missing/unsafe inventory directories")
    jobs = {}
    members = {}
    for path in sorted((root / "jobs").glob("*/record.json")):
        if len(jobs) >= MAX_JOBS:
            raise ValueError("inventory job limit reached; archive drained journals")
        job_id = path.parent.name
        if not ID.fullmatch(job_id) or path.is_symlink() or path.parent.is_symlink() or \
                path.stat().st_size > 131072:
            raise ValueError("invalid inventory journal")
        record = json.loads(path.read_text())
        if record["job"]["id"] != job_id or record["stage"] not in STAGES:
            raise ValueError("invalid inventory record")
        bundle = record.get("bundle")
        if bundle is not None:
            if not isinstance(bundle, str) or not ID.fullmatch(bundle):
                raise ValueError("invalid inventory record bundle")
            members.setdefault(bundle, []).append(job_id)
        jobs[job_id] = {"stage": record["stage"], "spool": None}
    # A bundle workunit counts as its pages, so the controller sees every page as it does a
    # single-page unit's.
    pending = list(pending_ids) + [j for b in pending_bundles for j in sorted(members.get(b, ()))]
    existing = list(existing_ids) + [j for b in existing_bundles for j in sorted(members.get(b, ()))]
    if len(set(pending)) != len(pending) or len(set(existing)) != len(existing):
        raise ValueError("a page is in more than one workunit")
    if len(existing) > existing_limit or len(existing_ids) + len(existing_bundles) > existing_limit:
        raise ValueError("invalid/oversized existing workunit inventory")
    seen = {}
    locations = {}
    candidates, spool_entries = spool_candidates(results, jobs, members)
    for path, match in candidates:
        if not path.resolve().is_relative_to(results.resolve()):
            raise ValueError("spool escapes results directory")
        key = (match.re is BUNDLE_SPOOL_FILE, match[1])
        locations.setdefault(key, set()).add(str(path.parent))
        if len(locations[key]) > 1:
            raise ValueError("multiple assimilator spools for one job, including partial outputs")
        seen.setdefault((key, str(path.parent)), set()).add(match[2])
    for ((bundled, name), directory), names in seen.items():
        if bundled:
            complete = {"__file_results.json", "__file_texts", "__file_layouts"} <= names
            owners = members[name]
        else:
            complete = {"__file_result.json", "__file_text.txt", "__file_layout.xml"} <= names
            owners = [name]
        if complete or names & {"_error", "_no_output_files"}:
            for job_id in owners:
                if jobs[job_id]["spool"] not in (None, directory):
                    raise ValueError("multiple assimilator spools for one job")
                jobs[job_id]["spool"] = directory
    payload = {
        "schema_version": 1,
        "nonce": nonce,
        "created_at": int(time.time()),
        "pending_workunits": counts[0],
        "pending_jobs": pending,
        # Pending OCR workunits, single-page and bundles: pending_jobs counts pages (#1059).
        "pending_ocr_workunits": len(pending_ids) + sum(bool(members.get(b)) for b in pending_bundles),
        "existing_jobs": existing,
        # Workunit rows, single-page and bundles, before bundles become pages. Bundles whose
        # journals are gone add no pages but still count towards the cap, so the controller warns
        # on the larger of this and existing_jobs (#1483).
        "existing_workunits": len(existing_ids) + len(existing_bundles),
        # Towards the existing-workunit cap, so the controller checks and warns against it (#1483).
        "existing_jobs_limit": existing_limit,
        "active_hosts": counts[1],
        "jobs": jobs,
        # Entries visited by the spool lookup, towards MAX_FILES, so the controller can warn
        # before the limit stops it (#1483): the entries of the results directory and the
        # job output files found. Other apps' results under the batch directories aren't visited
        # and aren't counted (#2008).
        "spool_entries": spool_entries,
        "spool_entries_limit": MAX_FILES,
    }
    if pending_by_app is not None:
        payload["pending_workunits_by_app"] = pending_by_app
    if app_versions is not None:
        payload["ocr_app_versions"] = app_versions
    if cpus is not None:
        # Host id (a string, as JSON keys are) -> its CPU count, active hosts only (#1780).
        payload["host_cpus"] = cpus
    if native_units is not None:
        # Host id -> its unassimilated usnm_ocr_native units and their job ids (#1780).
        payload["native_host_units"] = native_units
    if native_trusted is not None:
        # Build digest -> page language -> the hosts submission would admit (#1780). Without
        # it, a native plan with "native_hosts": "trusted" has no hosts.
        payload["native_trusted_hosts"] = native_trusted
    return payload


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--results", type=Path, required=True)
    parser.add_argument("--mysql-defaults-file", required=True)
    parser.add_argument("--database", required=True)
    parser.add_argument("--nonce", required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--native-trusted", type=Path,
                        help="usnm-boinc host-trust native-trusted's output (#1780)")
    args = parser.parse_args()
    if not re.fullmatch(r"[A-Za-z0-9_]+", args.database):
        parser.error("invalid database name")
    existing_limit = max_existing_workunits()
    output = subprocess.run(
        ["mysql", f"--defaults-extra-file={args.mysql_defaults_file}", "--ssl",
         "--ssl-verify-server-cert", "--ssl-ca=/usr/local/share/usnm/mysql-ca.pem",
         "--batch", "--raw", "--skip-column-names", f"--database={args.database}",
         "--execute", query(existing_limit)],
        check=True, text=True, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=60,
    ).stdout.strip().splitlines()
    hosts = output[0] if output else ""
    apps = [APP_ROW.fullmatch(value) for value in output[1:] if value.startswith("app\t")]
    versioned = [value for value in output[1:] if value.startswith(("minver\t", "version\t"))]
    cpus = host_cpus([value for value in output[1:] if value.startswith("host\t")])
    native_units = native_host_units([value for value in output[1:] if value.startswith("native\t")])
    names = [value for value in output[1:]
             if not value.startswith(("app\t", "minver\t", "version\t", "host\t", "native\t"))]
    native_trusted = None
    if args.native_trusted is not None:
        if args.native_trusted.stat().st_size > 16 << 20:
            raise ValueError("invalid native trusted hosts")
        native_trusted = native_trusted_hosts(json.loads(args.native_trusted.read_text()))
    if not hosts.isdecimal() or not all(apps) or len({app[1] for app in apps}) != len(apps) or \
            any(not re.fullmatch(r"usnm_(ocrb_)?[0-9a-f]{64}\t-?[0-9]+", value) for value in names):
        raise ValueError("invalid database inventory")
    rows = [value.split("\t") for value in names]
    # The query reads one row past the cap. Check the rows before bundles become their pages:
    # bundles whose journals are gone add no pages, so a full LIMIT of them would otherwise pass
    # with later workunits cut off.
    if len(rows) > existing_limit:
        raise ValueError("invalid/oversized existing workunit inventory")
    pages = [(name.removeprefix("usnm_"), int(state)) for name, state in rows
             if not name.startswith("usnm_" + BUNDLE_PREFIX)]
    bundles = [(name.removeprefix("usnm_" + BUNDLE_PREFIX), int(state)) for name, state in rows
               if name.startswith("usnm_" + BUNDLE_PREFIX)]
    counts = [sum(int(app[2]) for app in apps), int(hosts)]
    payload = inventory(args.root, args.results, counts, args.nonce,
                        [name for name, state in pages if state != 2],
                        [name for name, _ in pages],
                        [name for name, state in bundles if state != 2],
                        [name for name, _ in bundles],
                        pending_by_app={app[1]: int(app[2]) for app in apps if app[1]},
                        existing_limit=existing_limit, app_versions=ocr_app_versions(versioned),
                        cpus=cpus, native_units=native_units, native_trusted=native_trusted)
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(payload, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()
