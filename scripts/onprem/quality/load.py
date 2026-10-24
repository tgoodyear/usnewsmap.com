#!/usr/bin/env python3
"""Loads OCR-quality results into usnm_quality (#962), as quality_loader.

Runs where the files are read, on-prem (the batch container CT215, which mounts the curated
copy read-only); it writes only the database. Rows go through psql (COPY into a temporary
table, then INSERT ... ON CONFLICT), so a rerun replaces what it loaded before.

  load.py damage --dsn DSN --curated DIR [--batch-limit N] [--files-per-session 40]
  load.py results --dsn DSN --campaign NAME --engine sha256:... --file F [--file F ...]
  load.py ocr --dsn DSN --curated DIR
  load.py staging --dsn DSN --campaign NAME --docs FILE --image-source S --alto-source S

damage   every audit/damage/v2/<batch>_verNN.parquet: the page rows (quality.pages) and the
         damage score (quality.damage), under the campaign damage-v2. A page in two batch
         versions keeps the later file's row.
results  quality collect's pages.parquet or the wave runner's collected parts (one row per
         page and reader): quality.unit_pages and quality.reader_scores.
ocr      curated ocr-local/pages: one quality.ocr_outputs row per published run.
staging  which pages a campaign staged, with the image and ALTO they were staged from.

DSN is a libpq connection string with the client certificate, for example
"host=usnm-queue.tjg.fyi dbname=usnm_quality user=quality_loader sslmode=verify-full
sslcert=... sslkey=... sslrootcert=...".
"""
import argparse
import csv
import glob
import io
import json
import os
import re
import subprocess
import sys

CAMPAIGN_RE = re.compile(r"^[a-z0-9][a-z0-9.-]{0,63}$")
DOC_RE = re.compile(r"^[a-z0-9]+_\d{4}-\d{2}-\d{2}_ed-\d+_seq-\d+$")
OURS = ("ours-tesseract", "ours-surya", "ours-ndlocr", "ours-paddle")

UNIT_COLS = [
    "campaign", "doc_id", "job_id", "grp", "language", "outcome", "width", "height",
    "skew_deg", "blur", "contrast", "noise", "bleed_through", "ink_density", "blocks", "lines",
    "line_words", "columns", "line_height_px", "mean_word_conf", "consensus_words",
    "hat_determined", "hat_residual", "legibility_ceiling", "readers", "has_alto", "total_ms",
]
SCORE_COLS = [
    "campaign", "doc_id", "reader", "source", "chars", "words", "est_cer", "est_cer_raw",
    "completeness", "precision", "findable_content", "findable_places", "cer_vs_loc",
    "wer_vs_loc", "dice_vs_loc", "pairs",
]
PAGE_COLS = ["doc_id", "batch", "batch_version", "lccn", "date", "edition", "seq", "lang"]
DAMAGE_COLS = [
    "doc_id", "campaign", "text_status", "decision", "lang", "script", "damage_rate",
    "function_share", "garbage_share", "words", "head", "near",
]
OCR_COLS = ["doc_id", "reader", "engine", "source", "ocred_at", "text_chars", "damage_rate"]
STAGING_COLS = ["campaign", "doc_id", "image_source", "alto_source"]


def num(v):
    """A float from a number or a numeric string (the wave runner wrote some as strings)."""
    if v is None or v == "":
        return None
    try:
        f = float(v)
    except (TypeError, ValueError):
        return None
    return f if f == f else None  # NaN is NULL


def integer(v):
    f = num(v)
    return None if f is None else int(f)


def reader_name(engine, source):
    low = f"{engine or ''} {source or ''}".lower()
    for name in ("surya", "ndlocr", "paddle"):
        if name in low:
            return f"ours-{name}"
    return "ours-tesseract"


def csv_rows(rows):
    buf = io.StringIO()
    w = csv.writer(buf, lineterminator="\n")
    for r in rows:
        w.writerow(["" if v is None else ("t" if v is True else "f" if v is False else v) for v in r])
    return buf.getvalue()


def upsert_sql(table, cols, key, rows, update=True):
    """One transaction: COPY the rows into a temporary copy of the table, then upsert them."""
    if not rows:
        return ""
    collist = ", ".join(cols)
    rest = [c for c in cols if c not in key]
    if update and rest:
        action = "DO UPDATE SET " + ", ".join(f"{c} = EXCLUDED.{c}" for c in rest)
    else:
        action = "DO NOTHING"
    return (
        "BEGIN;\n"
        f"CREATE TEMP TABLE t_load ON COMMIT DROP AS SELECT {collist} FROM quality.{table} WITH NO DATA;\n"
        f"COPY t_load ({collist}) FROM STDIN WITH (FORMAT csv);\n"
        + csv_rows(rows)
        + "\\.\n"
        f"INSERT INTO quality.{table} ({collist}) SELECT {collist} FROM t_load "
        f"ON CONFLICT ({', '.join(key)}) {action};\n"
        "COMMIT;\n"
    )


def psql(dsn, script):
    if not script:
        return
    p = subprocess.run(["psql", dsn, "-X", "-q", "-v", "ON_ERROR_STOP=1"], input=script, text=True,
                       capture_output=True)
    if p.returncode:
        sys.exit(f"psql failed: {p.stderr.strip()[-800:]}")


def campaign_sql(campaign, source, engine=None, notes=None):
    rows = [(campaign, source, engine, notes)]
    return upsert_sql("campaigns", ["campaign", "source", "engine", "notes"], ["campaign"], rows)


def log(msg, **kv):
    print(json.dumps({"msg": msg, **kv}, sort_keys=True), flush=True)


# --- damage ---------------------------------------------------------------------------


def damage_rows(table, campaign):
    pages, damage = {}, {}
    for x in table:
        d = x["doc_id"]
        if not DOC_RE.match(d or ""):
            continue
        pages[d] = (d, x["batch"], x["batch_version"], x["lccn"], x["date"], x["edition"], x["seq"], x.get("lang"))
        damage[d] = (d, campaign, x.get("text_status"), x.get("decision"), x.get("lang"), x.get("script"),
                     num(x.get("damage_rate")), num(x.get("function_share")), num(x.get("garbage_share")),
                     integer(x.get("words")), integer(x.get("head")), integer(x.get("near")))
    return list(pages.values()), list(damage.values())


def cmd_damage(a):
    import pyarrow.parquet as pq

    files = sorted(glob.glob(os.path.join(a.curated, "audit/damage/v2", "*_ver[0-9][0-9].parquet")))
    if a.batch_limit:
        files = files[: a.batch_limit]
    if not files:
        sys.exit("no damage files")
    psql(a.dsn, campaign_sql("damage-v2", "damage", notes="crates/usnm-core damage v2, curated/audit/damage/v2"))
    cols = ["doc_id", "lccn", "date", "edition", "seq", "batch", "batch_version", "text_status", "lang",
            "script", "decision", "damage_rate", "function_share", "garbage_share", "words", "head", "near"]
    total = 0
    for i in range(0, len(files), a.files_per_session):
        script = []
        for fp in files[i : i + a.files_per_session]:
            pages, damage = damage_rows(pq.read_table(fp, columns=cols).to_pylist(), "damage-v2")
            script.append(upsert_sql("pages", PAGE_COLS, ["doc_id"], pages))
            script.append(upsert_sql("damage", DAMAGE_COLS, ["doc_id"], damage))
            total += len(pages)
        psql(a.dsn, "".join(script))
        log("damage loaded", files=min(i + a.files_per_session, len(files)), of=len(files), rows=total)


# --- results --------------------------------------------------------------------------


def result_rows(table, campaign):
    """unit_pages and reader_scores rows from one file's (page, reader) rows."""
    by_doc = {}
    for x in table:
        by_doc.setdefault(x["doc_id"], []).append(x)
    units, scores = [], []
    for d, rows in by_doc.items():
        if not DOC_RE.match(d or ""):
            continue
        x = rows[0]
        blocks = integer(x.get("blocks"))
        units.append((
            campaign, d, x.get("job_id"), x.get("group"), x.get("language"), x.get("outcome"),
            integer(x.get("width")), integer(x.get("height")), num(x.get("skew_deg")), num(x.get("blur")),
            num(x.get("contrast")), num(x.get("noise")), num(x.get("bleed_through")), num(x.get("ink_density")),
            blocks, integer(x.get("lines")), integer(x.get("line_words")), integer(x.get("columns")),
            num(x.get("line_height_px")), num(x.get("mean_word_conf")), integer(x.get("consensus_words")),
            x.get("hat_determined"), num(x.get("hat_residual")), num(x.get("legibility_ceiling")),
            len({r.get("reader") for r in rows}), bool(blocks), integer(x.get("total_ms")),
        ))
        seen = set()
        for r in rows:
            name = r.get("reader")
            if not name or name in seen:
                continue
            seen.add(name)
            pairs = r.get("pairs_json")
            scores.append((
                campaign, d, name, r.get("source"), integer(r.get("chars")), integer(r.get("words")),
                num(r.get("est_cer")), num(r.get("est_cer_raw")), num(r.get("completeness")),
                num(r.get("precision")), num(r.get("findable_content")), num(r.get("findable_places")),
                num(r.get("cer_vs_loc")), num(r.get("wer_vs_loc")), num(r.get("dice_vs_loc")),
                json.dumps(json.loads(pairs), sort_keys=True) if pairs else None,
            ))
    return units, scores


def cmd_results(a):
    import pyarrow.parquet as pq

    if not CAMPAIGN_RE.match(a.campaign):
        sys.exit("--campaign: a campaign name")
    psql(a.dsn, campaign_sql(a.campaign, "boinc-quality", engine=a.engine))
    units_n = scores_n = 0
    for fp in a.file:
        t = pq.read_table(fp).to_pylist()
        units, scores = result_rows(t, a.campaign)
        readers = sorted({s[2] for s in scores})
        psql(a.dsn, upsert_sql("readers", ["reader", "kind"], ["reader"],
                               [(r, "ours" if r.startswith("ours-") else "loc" if r.startswith("loc") else
                                 "american-stories" if r == "as" else "vlm") for r in readers], update=False)
             + upsert_sql("unit_pages", UNIT_COLS, ["campaign", "doc_id"], units)
             + upsert_sql("reader_scores", SCORE_COLS, ["campaign", "doc_id", "reader"], scores))
        units_n += len(units)
        scores_n += len(scores)
    log("results loaded", campaign=a.campaign, files=len(a.file), pages=units_n, reader_rows=scores_n)


# --- ocr ------------------------------------------------------------------------------


def cmd_ocr(a):
    import pyarrow.parquet as pq

    psql(a.dsn, campaign_sql("ocr-local", "ocr", notes="curated ocr-local/pages"))
    files = sorted(glob.glob(os.path.join(a.curated, "ocr-local/pages", "*", "*.parquet")))
    rows = {}
    cols = ["doc_id", "ocr_engine", "ocr_source", "ocred_at", "text_chars", "ours_damage_rate"]
    for fp in files:
        for x in pq.read_table(fp, columns=cols).to_pylist():
            if not DOC_RE.match(x["doc_id"] or "") or x["ocred_at"] is None:
                continue
            r = reader_name(x["ocr_engine"], x["ocr_source"])
            rows[(x["doc_id"], r, x["ocred_at"])] = (x["doc_id"], r, x["ocr_engine"], x["ocr_source"],
                                                    x["ocred_at"].isoformat(), x["text_chars"],
                                                    num(x["ours_damage_rate"]))
    psql(a.dsn, upsert_sql("ocr_outputs", OCR_COLS, ["doc_id", "reader", "ocred_at"], list(rows.values())))
    log("ocr loaded", files=len(files), rows=len(rows))


def cmd_staging(a):
    if not CAMPAIGN_RE.match(a.campaign):
        sys.exit("--campaign: a campaign name")
    with open(a.docs) as f:
        docs = sorted({line.split("\t", 1)[0].strip() for line in f if DOC_RE.match(line.split("\t", 1)[0].strip())})
    rows = [(a.campaign, d, a.image_source, a.alto_source) for d in docs]
    script = campaign_sql(a.campaign, "boinc-quality") if a.create_campaign else ""
    psql(a.dsn, script + upsert_sql("staging", STAGING_COLS, ["campaign", "doc_id"], rows))
    log("staging loaded", campaign=a.campaign, pages=len(rows))


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    d = sub.add_parser("damage")
    d.add_argument("--curated", required=True)
    d.add_argument("--batch-limit", type=int, default=0)
    d.add_argument("--files-per-session", type=int, default=40)
    r = sub.add_parser("results")
    r.add_argument("--campaign", required=True)
    r.add_argument("--engine")
    r.add_argument("--file", action="append", required=True)
    o = sub.add_parser("ocr")
    o.add_argument("--curated", required=True)
    s = sub.add_parser("staging")
    s.add_argument("--campaign", required=True)
    s.add_argument("--docs", required=True)
    s.add_argument("--image-source", required=True)
    s.add_argument("--alto-source", required=True)
    s.add_argument("--create-campaign", action="store_true")
    for p in (d, r, o, s):
        p.add_argument("--dsn", required=True)
    a = ap.parse_args(argv)
    {"damage": cmd_damage, "results": cmd_results, "ocr": cmd_ocr, "staging": cmd_staging}[a.cmd](a)


if __name__ == "__main__":
    main()
