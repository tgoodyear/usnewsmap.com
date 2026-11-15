#!/usr/bin/env python3
"""Selects the pages of a corpus-wide Tesseract re-read, worst first (usnewsmap.com#1999, part A).

  tess_worstfirst.py select --campaign ID --dsn-file FILE --out-dir DIR
      (--top-n N | --lm-oov-min X) [--language eng] [--min-words 100] [--chunk-pages 3000]

Reads quality.qe_loc_scores (qe-v1 scores of LoC's own OCR) read-only, on the replica, and writes
under --out-dir only: nothing is queued or shipped. The cut is a parameter the Coordinator sets:
--top-n takes the N pages with the highest lm_oov, --lm-oov-min every page at or above that value.
Both rank by lm_oov, worst first, then by doc_id so a rerun is the same list.

A page is a candidate when its row is valid, qe-v1, language_source `page` (never title_fallback
or none), has an lm_oov and at least --min-words words (a nearly blank page ranks worst on
lm_oov and is not worth a read), and its detected language is the campaign's. Left out:
pages already read by our Tesseract (quality.ocr_outputs, quality.boinc_page_provenance),
and pages held (quality.boinc_page_holds). A reading revoked for its host stays in
provenance, so revoked pages are left out too. Pages the ledger has outstanding are skipped
by ocr-enqueue itself.

Languages: only those in ALLOWED_LANGUAGES. Another language needs the Coordinator's sign-off
and an edit here. The frak2021 engine is never an option (this selects pages, not engines).

Outputs: pages.tsv (rank, doc_id, lm_oov, words, decile), chunks/enqueue-NNNN.txt (comma
separated doc_ids, at most --chunk-pages, for `ocr-enqueue --pages "$(cat FILE)" --reason bulk
--plan CAMPAIGN`; the argument limit of one argument is 128 KiB, so 3000 pages of about 35
characters), and summary.json (cut, counts, exclusions, coverage). `decile` is the page's decile
(1 = best, 10 = worst) of lm_oov over ALL scored pages of the language, the axis of the gain report.
"""
import argparse
import datetime
import fcntl
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys

ALLOWED_LANGUAGES = frozenset({"eng"})
FORBIDDEN_PROFILES = frozenset({"frak2021"})
QE_VERSION = "qe-v1"
CAMPAIGN_RE = re.compile(r"^[a-z0-9][a-z0-9._-]{2,79}$")
LANG_RE = re.compile(r"^[a-z]{3}$")


def check_language(language):
    if language in FORBIDDEN_PROFILES or language not in ALLOWED_LANGUAGES:
        raise SystemExit(f"language {language!r} is not allowed: only {sorted(ALLOWED_LANGUAGES)} "
                         "(anything else needs the Coordinator's sign-off; never frak2021)")


def select_sql(language, min_words, top_n, lm_oov_min, worst_fraction=None):
    """The query. Every value is validated, so it is formatted in: psql runs it read-only."""
    check_language(language)
    if not LANG_RE.match(language):
        raise SystemExit("bad language")
    if [top_n, lm_oov_min, worst_fraction].count(None) != 2:
        raise SystemExit("give exactly one of --top-n, --lm-oov-min and --worst-fraction")
    cut = f"AND lm_oov >= {float(lm_oov_min)!r}" if lm_oov_min is not None else ""
    if worst_fraction is not None:
        f = float(worst_fraction)
        if not 0 < f <= 1:
            raise SystemExit("worst fraction is in (0, 1]")
        cut = f"AND lm_oov >= (SELECT percentile_cont({1 - f!r}) WITHIN GROUP (ORDER BY lm_oov) FROM scored)"
    limit = f"LIMIT {int(top_n)}" if top_n is not None else ""
    return f"""
WITH scored AS (
  SELECT doc_id, lm_oov, words,
         ntile(10) OVER (ORDER BY lm_oov, doc_id) AS decile
  FROM quality.qe_loc_scores
  WHERE NOT invalid AND qe_version = '{QE_VERSION}' AND language = '{language}'
    AND language_source = 'page' AND lm_oov IS NOT NULL AND words >= {int(min_words)}
), cand AS (
  SELECT s.* FROM scored s
  WHERE EXISTS (SELECT 1 FROM quality.pages p WHERE p.doc_id = s.doc_id AND p.lang = '{language}')
    {cut}
    AND NOT EXISTS (SELECT 1 FROM quality.ocr_outputs o WHERE o.doc_id = s.doc_id AND o.reader = 'ours-tesseract')
    AND NOT EXISTS (SELECT 1 FROM quality.boinc_page_provenance v WHERE v.doc_id = s.doc_id)
    AND NOT EXISTS (SELECT 1 FROM quality.boinc_page_holds h WHERE h.doc_id = s.doc_id)
)
SELECT row_number() OVER (ORDER BY lm_oov DESC, doc_id), doc_id, lm_oov, words, decile
FROM cand ORDER BY lm_oov DESC, doc_id {limit}"""


def counts_sql(language, min_words):
    return f"""
SELECT
 (SELECT count(*) FROM quality.qe_loc_scores WHERE NOT invalid AND qe_version = '{QE_VERSION}' AND language = '{language}'),
 (SELECT count(*) FROM quality.qe_loc_scores WHERE NOT invalid AND qe_version = '{QE_VERSION}' AND language = '{language}'
    AND language_source = 'page' AND lm_oov IS NOT NULL AND words >= {int(min_words)}),
 (SELECT count(*) FROM quality.pages WHERE lang = '{language}')"""


def psql(dsn, sql, out=None):
    env = dict(os.environ, PGOPTIONS="-c default_transaction_read_only=on")
    cmd = ["psql", dsn, "-X", "-q", "-At", "-F", "\t", "-v", "ON_ERROR_STOP=1", "-c", sql]
    p = subprocess.run(cmd, stdout=out or subprocess.PIPE, stderr=subprocess.PIPE, env=env, text=out is None)
    if p.returncode:
        raise SystemExit("psql failed: " + (p.stderr if isinstance(p.stderr, str) else p.stderr.decode())[:300])
    return p.stdout


def chunk(ids, size):
    return [ids[i:i + size] for i in range(0, len(ids), size)]


def cmd_select(a):
    if not CAMPAIGN_RE.match(a.campaign):
        raise SystemExit("--campaign must be lower-case letters, digits, . _ -")
    if not 1 <= a.chunk_pages <= 3000:
        raise SystemExit("--chunk-pages is 1 to 3000")
    dsn = open(a.dsn_file).read().strip()
    sql = select_sql(a.language, a.min_words, a.top_n, a.lm_oov_min, a.worst_fraction)
    os.makedirs(os.path.join(a.out_dir, "chunks"), exist_ok=True)
    rows = [r.split("\t") for r in psql(dsn, sql).splitlines() if r]
    scored, usable, pages = (int(x) for x in psql(dsn, counts_sql(a.language, a.min_words)).split("\t"))
    ids = [r[1] for r in rows]
    with open(os.path.join(a.out_dir, "pages.tsv"), "w") as f:
        f.write("rank\tdoc_id\tlm_oov\twords\tdecile\n")
        for r in rows:
            f.write("\t".join(r) + "\n")
    names = []
    for n, part in enumerate(chunk(ids, a.chunk_pages), 1):
        name = f"enqueue-{n:04d}.txt"
        with open(os.path.join(a.out_dir, "chunks", name), "w") as f:
            f.write(",".join(part))
        names.append(name)
    by_decile = {}
    for r in rows:
        by_decile[r[4]] = by_decile.get(r[4], 0) + 1
    summary = {
        "campaign": a.campaign, "language": a.language, "qe_version": QE_VERSION,
        "cut": {"top_n": a.top_n, "lm_oov_min": a.lm_oov_min, "min_words": a.min_words},
        "selected": len(ids),
        "lm_oov_range": [float(rows[-1][2]), float(rows[0][2])] if rows else None,
        "by_decile": by_decile,
        "scored_valid_in_language": scored, "usable_after_filters": usable,
        "pages_in_language": pages, "coverage": round(scored / pages, 5) if pages else None,
        "chunks": names,
        "pages_tsv_sha256": hashlib.sha256(open(os.path.join(a.out_dir, "pages.tsv"), "rb").read()).hexdigest(),
    }
    with open(os.path.join(a.out_dir, "summary.json"), "w") as f:
        json.dump(summary, f, indent=2, sort_keys=True)
        f.write("\n")
    print(json.dumps(summary, sort_keys=True))


# ---------------------------------------------------------------------------------------------
# run: the timer job (usnm-tess-worstfirst.timer on usnm-batch). One run does one step:
# top up the current wave by one chunk, or close a finished wave and decide on the next.

DEFAULTS = {
    "language": "eng",
    "campaign_prefix": "tess-worstfirst",
    "cut": {"worst_fraction": 0.30},      # interim cut; the Science Coordinator edits this
    "min_words": 100,
    "wave_sizes": [20000, 50000, 100000],  # the last size repeats
    "chunk_pages": 3000,
    "max_outstanding": 60000,             # queued by the campaign, not yet published
    "min_pass_rate": 0.8,
    "complete_fraction": 0.95,
    "max_wave_days": 21,
    "pause_after": "2026-11-18T14:00:00Z",  # 18 Nov 9:00 AM ET
    "rebuild_done_marker": "/var/lib/usnm-tess-worstfirst/rebuild-done",
    "dsn_file": "/etc/usnm/quality-reader/dsn",
    "ingest_run": "/usr/local/bin/usnm-ingest-run",
    "tracking_issue": None,               # "owner/repo#N": one comment when the job stops itself
}


def now_utc():
    return datetime.datetime.now(datetime.timezone.utc)


def iso(t):
    return t.strftime("%Y-%m-%dT%H:%M:%SZ")


def wave_size(cfg, n):
    sizes = cfg["wave_sizes"]
    return sizes[min(n, len(sizes)) - 1]


def decide(cfg, st, now, published, gain):
    """The step this run takes. Pure: `published` is how many of the wave's pages are published,
    `gain` the measured {"pass_rate", "median_gain"} of the wave or None. Returns (action, why)."""
    if st.get("stopped"):
        return "idle", "stopped: " + st["stopped"]
    if now >= datetime.datetime.strptime(cfg["pause_after"], "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=datetime.timezone.utc) \
            and not os.path.exists(cfg["rebuild_done_marker"]):
        return "idle", "paused until " + cfg["rebuild_done_marker"] + " exists"
    n = st["wave"]
    target = wave_size(cfg, n)
    if st.get("exhausted"):
        return "stop", "no eligible pages left (wave %d had %d queued)" % (n, st["wave_queued"])
    started = datetime.datetime.strptime(st["wave_started"], "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=datetime.timezone.utc)
    age_days = (now - started).total_seconds() / 86400
    timed_out = age_days >= cfg["max_wave_days"]
    if st["wave_queued"] < target and not timed_out:
        if st["outstanding"] >= cfg["max_outstanding"]:
            return "idle", "queue full: %d outstanding" % st["outstanding"]
        return "enqueue", "wave %d: %d of %d" % (n, st["wave_queued"], target)
    done = published >= cfg["complete_fraction"] * max(st["wave_queued"], 1) or timed_out
    if not done:
        return "idle", "wave %d: %d of %d queued pages published" % (n, published, st["wave_queued"])
    if gain is None:
        return "idle", "wave %d done; waiting for its qe gain (gain/%s.json)" % (n, st["campaign"])
    if gain["pass_rate"] < cfg["min_pass_rate"]:
        return "stop", "wave %d gate pass rate %.3f is below %.2f" % (n, gain["pass_rate"], cfg["min_pass_rate"])
    if gain["median_gain"] <= 0:
        return "stop", "wave %d median qe gain %.4f is not above 0" % (n, gain["median_gain"])
    return "next_wave", "wave %d passed (pass rate %.3f, median gain %.4f)" % (n, gain["pass_rate"], gain["median_gain"])


def load_json(path, default):
    try:
        with open(path) as f:
            return json.load(f)
    except FileNotFoundError:
        return default


def save_json(path, obj):
    tmp = path + ".part"
    with open(tmp, "w") as f:
        json.dump(obj, f, indent=2, sort_keys=True)
        f.write("\n")
    os.replace(tmp, path)


def published_count(dsn, ids):
    total = 0
    for part in chunk(ids, 3000):
        sql = ("SELECT count(DISTINCT doc_id) FROM (SELECT doc_id FROM quality.boinc_page_provenance "
               "UNION ALL SELECT doc_id FROM quality.ocr_outputs WHERE reader = 'ours-tesseract') x "
               "WHERE doc_id = ANY(string_to_array('%s', ','))" % ",".join(part))
        if not all(re.match(r"^[A-Za-z0-9._-]+$", i) for i in part):
            raise SystemExit("odd doc_id in wave file")
        total += int(psql(dsn, sql).strip())
    return total


def enqueue(cfg, ids, campaign, dry_run=False):
    """Runs ocr-enqueue on this host. Returns {doc_id: why} for the pages it did not queue (outstanding
    already, or not in the published version); the rest of `ids` are queued."""
    p = subprocess.run([cfg["ingest_run"], "--command", "ocr-enqueue", "--wait", "--nice", "19", "--",
                        "--pages", ",".join(ids), "--reason", "bulk", "--plan", campaign] + (["--dry-run"] if dry_run else []),
                       stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, timeout=3600)
    report = None
    for line in p.stdout.splitlines():
        if '"ocr-enqueue finished"' in line or '"ocr-enqueue dry run"' in line:
            report = json.loads(line)["fields"]
    if p.returncode or report is None:
        raise RuntimeError("ocr-enqueue failed (%s): %s" % (p.returncode, p.stdout[-400:]))
    skipped = {}
    for line in p.stdout.splitlines():
        if '"page not queued"' in line:
            f = json.loads(line)["fields"]
            skipped[f["doc_id"]] = f.get("why")
    return skipped


def stop_self(cfg, state_dir, st, why, log):
    st["stopped"] = why
    st["stopped_at"] = iso(now_utc())
    log("STOPPED " + why)
    text = "tess-worstfirst stopped itself: %s (campaign %s, state %s)" % (why, st["campaign"], state_dir)
    issue = cfg.get("tracking_issue")
    if issue and shutil.which("gh"):
        repo, num = issue.split("#")
        r = subprocess.run(["gh", "issue", "comment", num, "--repo", repo, "--body", text])
        if r.returncode == 0:
            return
    with open(os.path.join(state_dir, "STOPPED"), "w") as f:
        f.write(text + "\n")


def cmd_run(a):
    cfg = dict(DEFAULTS, **load_json(a.config, {}))
    check_language(cfg["language"])
    state_dir = a.state_dir
    os.makedirs(os.path.join(state_dir, "waves"), exist_ok=True)
    os.makedirs(os.path.join(state_dir, "gain"), exist_ok=True)
    lock = open(os.path.join(state_dir, "lock"), "w")
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        print("another run holds the lock")
        return

    def log(msg):
        line = "%s tess-worstfirst %s" % (iso(now_utc()), msg)
        print(line)
        with open(os.path.join(state_dir, "run.log"), "a") as f:
            f.write(line + "\n")

    sp = os.path.join(state_dir, "state.json")
    st = load_json(sp, None)
    if st is None:
        st = {"wave": 1, "wave_queued": 0, "wave_skipped": 0, "outstanding": 0, "exhausted": False,
              "wave_started": iso(now_utc())}
    st["campaign"] = "%s-%s-w%d" % (cfg["campaign_prefix"], cfg["language"], st["wave"])
    wave_file = os.path.join(state_dir, "waves", "w%d.txt" % st["wave"])
    wave_ids = open(wave_file).read().split() if os.path.exists(wave_file) else []
    dsn = open(cfg["dsn_file"]).read().strip()
    published = published_count(dsn, wave_ids) if wave_ids else 0
    st["published"] = published
    st["outstanding"] = max(st["wave_queued"] - published, 0)
    gain = load_json(os.path.join(state_dir, "gain", st["campaign"] + ".json"), None)
    action, why = decide(cfg, st, now_utc(), published, gain)

    if action == "enqueue":
        seen = set()
        for fn in os.listdir(os.path.join(state_dir, "waves")):
            seen.update(open(os.path.join(state_dir, "waves", fn)).read().split())
        remaining = wave_size(cfg, st["wave"]) - st["wave_queued"]
        want = min(cfg["chunk_pages"], remaining)
        cut = cfg["cut"]
        # Rank the pages, leave out those an earlier step queued, take the next `want`.
        args = dict(top_n=None, lm_oov_min=cut.get("lm_oov_min"), worst_fraction=cut.get("worst_fraction"))
        if "top_n" in cut:
            args = dict(top_n=cut["top_n"], lm_oov_min=None, worst_fraction=None)
        sql = select_sql(cfg["language"], cfg["min_words"], **args)
        if args["top_n"] is None:  # a bounded read: the next page of the ranking
            sql += " LIMIT %d" % (len(seen) + want)
        rows = [r.split("\t") for r in psql(dsn, sql).splitlines() if r]
        pending = os.path.join(state_dir, "pending.json")
        retry = os.path.exists(pending)
        if retry:  # a chunk whose send failed: send the same pages again first
            p_ = load_json(pending, {})
            fresh, before = p_["ids"], set(p_["outstanding_before"])
        else:
            fresh = [r[1] for r in rows if r[1] not in seen][:want]
            before = set()
            if fresh:  # which pages were outstanding before our send, so a retry knows which are ours
                before = {i for i, w in enqueue(cfg, fresh, st["campaign"], dry_run=True).items() if w == "outstanding"}
                save_json(pending, {"ids": fresh, "outstanding_before": sorted(before)})
        if not fresh:
            st["exhausted"] = True
            log("no eligible pages left past %d" % len(seen))
        else:
            try:
                skipped = enqueue(cfg, fresh, st["campaign"])
            except Exception as e:  # pending.txt stays: ocr-enqueue skips what already went out
                log("enqueue failed: %s" % e)
                save_json(sp, st)
                sys.exit(1)
            if retry:  # the failed attempt may have put some out: pages outstanding now but not before are ours
                skipped = {i: w for i, w in skipped.items() if w != "outstanding" or i in before}
            skipped_ids = set(skipped)
            queued_ids = [i for i in fresh if i not in skipped_ids]
            with open(wave_file, "a") as f:
                f.write("".join(i + "\n" for i in queued_ids))
            with open(wave_file[:-4] + ".skipped", "a") as f:
                f.write("".join(i + "\n" for i in sorted(skipped_ids)))
            os.remove(pending)
            st["wave_queued"] += len(queued_ids)
            st["wave_skipped"] += len(skipped_ids)  # already queued by the seeder, or not queueable: no campaign tag
            st["outstanding"] = max(st["wave_queued"] - published, 0)
            log("%s: queued %d, not queued %d (wave %d/%d)" % (
                st["campaign"], len(queued_ids), len(skipped_ids),
                st["wave_queued"] + st["wave_skipped"], wave_size(cfg, st["wave"])))
    elif action == "next_wave":
        with open(os.path.join(state_dir, "waves.jsonl"), "a") as f:
            f.write(json.dumps(dict(gain, campaign=st["campaign"], queued=st["wave_queued"],
                                    published=published, closed_at=iso(now_utc())), sort_keys=True) + "\n")
        st.update(wave=st["wave"] + 1, wave_queued=0, wave_skipped=0, exhausted=False, wave_started=iso(now_utc()))
        log(why)
    elif action == "stop":
        stop_self(cfg, state_dir, st, why, log)
    else:
        log("idle: " + why)
    save_json(sp, st)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("select")
    s.add_argument("--campaign", required=True)
    s.add_argument("--dsn-file", required=True)
    s.add_argument("--out-dir", required=True)
    s.add_argument("--language", default="eng")
    s.add_argument("--top-n", type=int)
    s.add_argument("--lm-oov-min", type=float)
    s.add_argument("--worst-fraction", type=float, help="the worst fraction of the scored pages by lm_oov (0.30 = worst 30%%)")
    s.add_argument("--min-words", type=int, default=100)
    s.add_argument("--chunk-pages", type=int, default=3000)
    s.set_defaults(fn=cmd_select)
    r = sub.add_parser("run", help="one step of the timer job")
    r.add_argument("--config", required=True)
    r.add_argument("--state-dir", required=True)
    r.set_defaults(fn=cmd_run)
    a = ap.parse_args(argv)
    a.fn(a)


if __name__ == "__main__":
    sys.exit(main())
