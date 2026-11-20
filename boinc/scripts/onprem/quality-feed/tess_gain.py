#!/usr/bin/env python3
"""Scores a Tesseract re-read campaign with qe-v1 and writes its gain (usnewsmap.com#1999, part C).

  tess_gain.py run --config FILE --state-dir DIR [--campaign ID]

The job beside tess_worstfirst.py (usnm-tess-gain.timer). tess_worstfirst.py run closes a wave only
when <state dir>/gain/<campaign>.json has `pass_rate` and `median_gain`; this writes that file.

Path taken: LOCAL. The published pages' text is scored here with the `usnm_quality` binary, the
text-only unit (usnm_ocr_contract::qe_text, usnm_quality 1.5) run in a scratch slot directory. The BOINC
pipeline (qe-text-stage, -submit, -collect; #1989, #1994) was not used because it cannot take a
tagged submission without changing it: stage reads one LoC bulk archive's extract and the
authoritative language list for that archive, and collect writes only quality.qe_loc_scores, the
table of LoC's own scores (append-only, one valid row a page and measure version, no campaign
column), so our text would have to be written into LoC's table. The unit and the score are the same
either way: the same job file, the same qe.bin, the same `Measure::report`.

What it reads, all read-only: the ledger (`queued`, `outcomes`) and the published pages in a
copy of the ocr-local dataset (`ocr_local_dir`, as ocr_manifest.py reads it), and
quality.qe_loc_scores and quality.boinc_page_holds from the replica (`dsn_file`, as the
selector does).

Pass rate, from the ledger and not a proxy: of the campaign's pages (queued rows with the campaign as
`plan`), those with a latest outcome after their latest queued row. `imported` is published, unless the
page is held (quality.boinc_page_holds); `dead_lettered` (any cause but `unroutable`, which the seeder
queues again) and `not_sent` are failures. A page with no outcome yet is pending and not in the rate.
  pass_rate = published / (published + held + dead_lettered + not_sent)

Gain, as quality_gain.py's drop (positive is better): the page's LoC lm_oov minus ours, for each
page with both (LoC's row is valid, qe-v1, language_source `page`; ours has words). lm_oov is the
decision measure of qe-v1, higher is worse. `median_gain` is the median over the paired pages; per decile of
LoC's lm_oov over every scored page of the language (1 = best, 10 = worst, as the selector's
`decile`) it also has the page count, median and mean.

The final gain/<campaign>.json is written only once the wave is done by the timer's own test
(published at least complete_fraction of queued, or max_wave_days old) and every published page is
scored, so the timer never reads a partial figure. Until then gain/<campaign>.progress.json says how far
it is. Scores are cached by page and text hash, so a rerun scores only what is new and writes the
same file; one run scores at most `score_pages_per_run` pages. One JSON line goes to stdout a run.
Like the selector, it does nothing from `pause_after` until the rebuild marker exists.
"""
import argparse
import datetime
import fcntl
import glob
import hashlib
import json
import os
import re
import shutil
import statistics
import subprocess
import sys
import tempfile

ALLOWED_LANGUAGES = frozenset({"eng"})
QE_VERSION = "qe-v1"
GROUP = "tess-gain"                       # qe_text's check_name: lower-case, digits, -, up to 32
CAMPAIGN_RE = re.compile(r"^[a-z0-9][a-z0-9._-]{2,79}$")
DOC_RE = re.compile(r"^[a-z0-9]+_\d{4}-\d{2}-\d{2}_ed-\d+_seq-\d+$")
OURS_SOURCE = "usnm-tesseract"
JOB_FILE, RESULT_FILE, QE_FILE = "qe_text.json", "qe_text_result.json", "qe.bin"
UNIT_VERSION = 1

# The keys it shares with tess_worstfirst.py's DEFAULTS keep their values (a test checks that).
DEFAULTS = {
    "language": "eng",
    "campaign_prefix": "tess-worstfirst",
    "complete_fraction": 0.95,
    "max_wave_days": 21,
    "pause_after": "2026-11-18T14:00:00Z",
    "rebuild_done_marker": "/var/lib/usnm-tess-worstfirst/rebuild-done",
    "dsn_file": "/etc/usnm/quality-reader/dsn",
    "ocr_local_dir": "/srv/curated/ocr-local",     # a copy of the ocr-local dataset: queued/, outcomes/, pages/
    "scorer": "/usr/local/bin/usnm_quality",        # usnm_quality 1.5 or later (text-only units)
    "qe_dir": "/usr/local/share/usnm-qe",           # <language>.qe, the qe-v1 resource files
    "qe_lock_file": None,                           # ocr/qe/resources.lock.json; its sha256 is the unit's `lock`
    "unit_pages": 5000,
    "score_pages_per_run": 20000,
}


def now_utc():
    return datetime.datetime.now(datetime.timezone.utc)


def iso(t):
    return t.strftime("%Y-%m-%dT%H:%M:%SZ")


def parse_time(s):
    return datetime.datetime.strptime(s, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=datetime.timezone.utc)


def sha256(b):
    return hashlib.sha256(b).hexdigest()


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


def check_language(language):
    if language not in ALLOWED_LANGUAGES:
        raise SystemExit(f"language {language!r} is not allowed: only {sorted(ALLOWED_LANGUAGES)}")


# ---------------------------------------------------------------------------------------------
# The ledger: pass rate.

def ledger_outcomes(queued, outcomes, campaign):
    """{doc_id: "imported" | "dead_lettered" | "not_sent" | "pending"} for the pages the campaign
    queued. As usnm-ocr-local's Ledger::outstanding (and ocr_manifest.pending): the page's newest
    queued row decides which campaign it is in, an outcome counts only when it is not older than
    that row, and a dead letter for `unroutable` is still pending, for the seeder queues it again."""
    last = {}
    for q in queued:
        cur = last.get(q["doc_id"])
        if cur is None or q["queued_at"] > cur["queued_at"]:
            last[q["doc_id"]] = q
    latest = {}
    for o in outcomes:
        if not o.get("at"):
            continue
        cur = latest.get(o["doc_id"])
        if cur is None or o["at"] >= cur["at"]:
            latest[o["doc_id"]] = o
    out = {}
    for doc_id, q in last.items():
        if q.get("plan") != campaign:
            continue
        o = latest.get(doc_id)
        if o is None or o["at"] < q["queued_at"] or (
                o["outcome"] == "dead_lettered" and o.get("cause") == "unroutable"):
            out[doc_id] = "pending"
        else:
            out[doc_id] = o["outcome"]
    return out


def pass_rate(states, held):
    """(rate, counts, published ids). `held` is the set of doc_ids with a hold; a settled page that
    is held counts as held whatever its outcome."""
    c = {"queued": len(states), "published": 0, "held": 0, "dead_lettered": 0, "not_sent": 0, "pending": 0}
    published = set()
    for doc_id, s in states.items():
        if s == "pending":
            c["pending"] += 1
        elif doc_id in held:
            c["held"] += 1
        elif s == "imported":
            c["published"] += 1
            published.add(doc_id)
        elif s in ("dead_lettered", "not_sent"):
            c[s] += 1
        else:
            raise SystemExit(f"unknown ledger outcome {s!r}")
    settled = c["published"] + c["held"] + c["dead_lettered"] + c["not_sent"]
    return (c["published"] / settled if settled else None), c, published


# ---------------------------------------------------------------------------------------------
# The gain.

def decile_of(lm_oov, cuts):
    """1 (best) to 10 (worst): how many of the nine cut points are at or below the value, plus one."""
    return 1 + sum(1 for c in cuts if lm_oov >= c)


def gain_summary(pairs, cuts):
    """`pairs` is [(doc_id, loc_lm_oov, ours_lm_oov)]. Gain is LoC's minus ours: positive is better."""
    gains = [(loc - ours, loc) for _, loc, ours in pairs]
    by = {}
    for g, loc in gains:
        by.setdefault(decile_of(loc, cuts), []).append(g)
    return {
        "n_paired": len(gains),
        "median_gain": round(statistics.median(g for g, _ in gains), 6) if gains else None,
        "mean_gain": round(statistics.fmean(g for g, _ in gains), 6) if gains else None,
        "by_decile": {str(d): {"n": len(v), "median_gain": round(statistics.median(v), 6),
                               "mean_gain": round(statistics.fmean(v), 6)} for d, v in sorted(by.items())},
    }


def wave_done(cfg, st, now):
    """tess_worstfirst.decide's own test that the wave is over: the queued pages published to
    complete_fraction, or the wave timed out. `st` is the timer's state.json."""
    queued = st.get("wave_queued", 0)
    started = parse_time(st["wave_started"])
    timed_out = (now - started).total_seconds() / 86400 >= cfg["max_wave_days"]
    return timed_out or st.get("published", 0) >= cfg["complete_fraction"] * max(queued, 1)


# ---------------------------------------------------------------------------------------------
# The units: qe_text.json as usnm_ocr_contract::qe_text::QeTextJob.

def unit_job(campaign, language, lock, qe_bytes, pages):
    """`pages` is [(doc_id, text)]. The id is the job's identity: sha256 of the JSON of
    (v, archive, group, lock, language, pages, qe), fields in the struct's order."""
    pg = [{"doc_id": d, "language_source": "page", "text": t} for d, t in pages]
    qe = {"input": {"bytes": len(qe_bytes), "sha256": sha256(qe_bytes),
                    "source": "file:%s.qe" % language}, "language": language}
    ident = json.dumps([UNIT_VERSION, campaign, GROUP, lock, language, pg, qe],
                       separators=(",", ":"), ensure_ascii=False)
    return {"v": UNIT_VERSION, "id": sha256(ident.encode()), "archive": campaign, "group": GROUP,
            "lock": lock, "language": language, "pages": pg, "qe": qe}


def run_unit(cfg, job, qe_bytes, workdir):
    """Runs the scorer on one unit in a scratch slot. Returns {doc_id: lm_oov or None}."""
    slot = tempfile.mkdtemp(prefix="unit-", dir=workdir)
    try:
        with open(os.path.join(slot, JOB_FILE), "w", encoding="utf-8") as f:
            json.dump(job, f, separators=(",", ":"), ensure_ascii=False)
        with open(os.path.join(slot, QE_FILE), "wb") as f:
            f.write(qe_bytes)
        p = subprocess.run([cfg["scorer"]], cwd=slot, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                           text=True, timeout=3600)
        res_path = os.path.join(slot, RESULT_FILE)
        if p.returncode or not os.path.exists(res_path):
            raise RuntimeError("scorer failed (%s): %s" % (p.returncode, p.stdout[-300:]))
        with open(res_path, encoding="utf-8") as f:
            res = json.load(f)
        if res.get("id") != job["id"] or [r["doc_id"] for r in res["rows"]] != [x["doc_id"] for x in job["pages"]]:
            raise RuntimeError("scorer result is not for this unit")
        return {r["doc_id"]: (r["qe"] or {}).get("lm_oov") for r in res["rows"]}
    finally:
        shutil.rmtree(slot, ignore_errors=True)


def read_cache(path):
    cache = {}
    if os.path.exists(path):
        with open(path) as f:
            for line in f:
                if line.strip():
                    r = json.loads(line)
                    cache[r["doc_id"]] = r
    return cache


def score_pages(cfg, campaign, texts, cache_path, workdir, budget):
    """Scores the pages in `texts` ({doc_id: text}) that the cache (page and text hash) lacks,
    at most `budget`, a unit at a time, each appended to the cache as it is done. Returns
    (cache, scored_now, qe_sha256, lock)."""
    with open(os.path.join(cfg["qe_dir"], cfg["language"] + ".qe"), "rb") as f:
        qe_bytes = f.read()
    if cfg.get("qe_lock_file"):
        with open(cfg["qe_lock_file"], "rb") as f:
            lock = sha256(f.read())
    else:
        lock = sha256(qe_bytes)
    cache = read_cache(cache_path)
    todo = [d for d in sorted(texts) if d not in cache or cache[d]["sha"] != sha256(texts[d].encode())][:budget]
    scored = 0
    for i in range(0, len(todo), cfg["unit_pages"]):
        part = todo[i:i + cfg["unit_pages"]]
        got = run_unit(cfg, unit_job(campaign, cfg["language"], lock, qe_bytes, [(d, texts[d]) for d in part]),
                       qe_bytes, workdir)
        with open(cache_path, "a") as f:
            for d in part:
                r = {"doc_id": d, "sha": sha256(texts[d].encode()), "lm_oov": got[d]}
                cache[d] = r
                f.write(json.dumps(r, sort_keys=True) + "\n")
        scored += len(part)
    return cache, scored, sha256(qe_bytes), lock


# ---------------------------------------------------------------------------------------------
# Readers: the dataset copy and the replica.

def parquet_rows(path, columns):
    import pyarrow.parquet as pq
    return pq.read_table(path, columns=columns).to_pylist()


def ledger_rows(root, kind, columns):
    return [r for path in sorted(glob.glob(os.path.join(root, kind, "*", "*.parquet")))
            for r in parquet_rows(path, columns)]


def published_texts(root, doc_ids):
    """{doc_id: text}: the newest of our Tesseract rows of each page, from pages/<lccn>/*.parquet."""
    want = set(doc_ids)
    best = {}
    for lccn in sorted({d.split("_", 1)[0] for d in want}):
        for path in sorted(glob.glob(os.path.join(root, "pages", lccn, "*.parquet"))):
            for r in parquet_rows(path, ["doc_id", "ocr_source", "ocred_at", "text"]):
                if r["doc_id"] in want and r["ocr_source"] == OURS_SOURCE and r["text"]:
                    cur = best.get(r["doc_id"])
                    if cur is None or r["ocred_at"] >= cur[0]:
                        best[r["doc_id"]] = (r["ocred_at"], r["text"])
    return {d: t for d, (_, t) in best.items()}


def psql(dsn, sql):
    env = dict(os.environ, PGOPTIONS="-c default_transaction_read_only=on")
    p = subprocess.run(["psql", dsn, "-X", "-q", "-At", "-F", "\t", "-v", "ON_ERROR_STOP=1", "-c", sql],
                       stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env, text=True)
    if p.returncode:
        raise SystemExit("psql failed: " + p.stderr[:300])
    return p.stdout


def id_list_sql(ids):
    if not all(DOC_RE.match(i) for i in ids):
        raise SystemExit("odd doc_id in the ledger")
    return "string_to_array('%s', ',')" % ",".join(ids)


def held_pages(dsn, ids):
    ids = sorted(ids)
    out = set()
    for i in range(0, len(ids), 3000):
        sql = "SELECT DISTINCT doc_id FROM quality.boinc_page_holds WHERE doc_id = ANY(%s)" % id_list_sql(ids[i:i + 3000])
        out.update(r for r in psql(dsn, sql).split("\n") if r)
    return out


def loc_scores(dsn, ids, language):
    """({doc_id: lm_oov}, resource locks): LoC's valid qe-v1 rows (language_source page) for the pages."""
    check_language(language)
    ids = sorted(ids)
    scores, locks = {}, set()
    for i in range(0, len(ids), 3000):
        sql = ("SELECT doc_id, lm_oov, resources_lock FROM quality.qe_loc_scores "
               "WHERE NOT invalid AND qe_version = '%s' AND language = '%s' AND language_source = 'page' "
               "AND lm_oov IS NOT NULL AND doc_id = ANY(%s)" % (QE_VERSION, language, id_list_sql(ids[i:i + 3000])))
        for r in psql(dsn, sql).split("\n"):
            if r:
                d, v, lk = r.split("\t")
                scores[d] = float(v)
                locks.add(lk)
    return scores, locks


def decile_cuts(dsn, language):
    """The nine cut points of lm_oov over every scored page of the language (as the selector's ntile(10))."""
    check_language(language)
    sql = ("SELECT unnest(percentile_cont(ARRAY[0.1,0.2,0.3,0.4,0.5,0.6,0.7,0.8,0.9]) WITHIN GROUP (ORDER BY lm_oov)) "
           "FROM quality.qe_loc_scores WHERE NOT invalid AND qe_version = '%s' AND language = '%s' "
           "AND language_source = 'page' AND lm_oov IS NOT NULL" % (QE_VERSION, language))
    cuts = [float(x) for x in psql(dsn, sql).split()]
    if len(cuts) != 9:
        raise SystemExit("no decile cut points: no scored pages")
    return cuts


# ---------------------------------------------------------------------------------------------

def cmd_run(a):
    cfg = dict(DEFAULTS, **load_json(a.config, {}))
    check_language(cfg["language"])
    gain_dir = os.path.join(a.state_dir, "gain")
    os.makedirs(gain_dir, exist_ok=True)
    lock = open(os.path.join(gain_dir, "lock"), "w")
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        print(json.dumps({"job": "tess-gain", "status": "locked"}))
        return

    now = now_utc()
    report = {"at": iso(now), "job": "tess-gain"}

    def finish(status, code=0, **kw):
        report.update(status=status, **kw)
        print(json.dumps(report, sort_keys=True), flush=True)
        if code:
            sys.exit(code)

    if now >= parse_time(cfg["pause_after"]) and not os.path.exists(cfg["rebuild_done_marker"]):
        return finish("paused", why="until " + cfg["rebuild_done_marker"] + " exists")
    st = load_json(os.path.join(a.state_dir, "state.json"), None)
    campaign = a.campaign or (st or {}).get("campaign")
    if not campaign or not CAMPAIGN_RE.match(campaign):
        return finish("idle", why="no campaign yet (no state.json from tess_worstfirst.py)")
    report["campaign"] = campaign
    final = os.path.join(gain_dir, campaign + ".json")
    progress = os.path.join(gain_dir, campaign + ".progress.json")
    wave_over = a.campaign is not None or wave_done(cfg, st, now)
    dsn = open(cfg["dsn_file"]).read().strip()
    try:
        queued = ledger_rows(cfg["ocr_local_dir"], "queued", ["doc_id", "plan", "queued_at"])
        outcomes = ledger_rows(cfg["ocr_local_dir"], "outcomes", ["doc_id", "outcome", "cause", "at"])
        states = ledger_outcomes(queued, outcomes, campaign)
        held = held_pages(dsn, [d for d, s in states.items() if s != "pending"])
        rate, counts, published = pass_rate(states, held)
        report.update(counts)
        texts = published_texts(cfg["ocr_local_dir"], published)
        report["published_text_missing"] = len(published) - len(texts)
        cache, scored_now, qe_sha, unit_lock = score_pages(
            cfg, campaign, texts, os.path.join(gain_dir, campaign + ".scores.jsonl"), gain_dir,
            cfg["score_pages_per_run"])
        loc, loc_locks = loc_scores(dsn, list(texts), cfg["language"])
        cuts = decile_cuts(dsn, cfg["language"])
    except (RuntimeError, OSError, SystemExit) as e:
        return finish("failed", code=1, error=str(e)[:400])
    pairs = [(d, loc[d], cache[d]["lm_oov"]) for d in sorted(texts)
             if d in cache and d in loc and cache[d]["lm_oov"] is not None]
    unscored = sum(1 for d in texts if d not in cache or cache[d]["sha"] != sha256(texts[d].encode()))
    report.update(scored_now=scored_now, awaiting_score=unscored, paired=len(pairs))
    summary = gain_summary(pairs, cuts)
    body = dict(summary, campaign=campaign, language=cfg["language"], pass_rate=rate,
                measure=QE_VERSION + " lm_oov", gain="LoC lm_oov minus ours; positive is better",
                path="local: usnm_quality text-only unit", ours_resources_sha256=qe_sha,
                loc_resources_locks=sorted(loc_locks), unit_lock=unit_lock, ledger=counts,
                no_loc_score=sum(1 for d in texts if d not in loc),
                no_words=sum(1 for d in texts if d in cache and cache[d]["lm_oov"] is None))
    if not wave_over or unscored or rate is None or summary["median_gain"] is None:
        why = ("wave not done" if not wave_over else "pages await a score" if unscored
               else "no settled pages" if rate is None else "no paired pages")
        save_json(progress, dict(body, why=why, at=iso(now)))
        return finish("progress", why=why)
    save_json(final, dict(body, computed_at=iso(now)))
    if os.path.exists(progress):
        os.remove(progress)
    return finish("written", pass_rate=round(rate, 5), median_gain=summary["median_gain"])


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run", help="score the current campaign and write its gain")
    r.add_argument("--config", required=True)
    r.add_argument("--state-dir", required=True)
    r.add_argument("--campaign", help="a campaign to finish regardless of the wave's state (default: state.json's)")
    r.set_defaults(fn=cmd_run)
    a = ap.parse_args(argv)
    a.fn(a)


if __name__ == "__main__":
    sys.exit(main())
