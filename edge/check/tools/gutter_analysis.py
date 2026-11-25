"""Per-segment outcomes of the column gutter judgments (#2107), from check.usnewsmap.com's export.

  curl -s 'https://check.usnewsmap.com/api/export?lang=en-gutter' > check-en-gutter.jsonl
  python3 gutter_analysis.py --export check-en-gutter.jsonl [--set gutter-v1] [--out segments-out.jsonl] \\
      [--dump admin-dump.jsonl]

Reads /api/export rows of `kind` gutter. A settled task's outcome is `a` (yes: the line runs down a gap), `b` (no), `unsure`
(can't tell) or `disputed` (the answers never agreed). Each settled segment becomes one row in --out (default: stdout is only the
summary): segment_id, page_id, method, role, truth if it has one, the answer counts and the outcome as yes, no, unsure or
disputed. `method`, `role` and `truth` are in the export once a task has settled, never before; an unsettled task is counted and
has no row.

A task that settled as `bad_crop` (two thirds of its answers said Bad crop: the crop or line is a build defect) is never a
segment row and never unsure: it is counted as `left_out_build_defect` and should be rebuilt (bad_crops.py lists them).

Unsure and disputed segments are counted apart from yes and no: they are not in `decided`, and a later study should say how it
treats them. This tool computes no rates and no per-method figures; those are the #2107 study's.

--dump is an /admin/dump file (backup/README.md; needs the admin token). With it, a volunteer's accuracy on the calibration
items is reported: of the tasks whose private body has a `truth`, how many of that volunteer's yes or no answers matched it,
how many did not, and how many were can't tell. A Bad crop answer is about the crop, not the line, and is in none of these. Contributor ids are cut to their first 8 characters. Answers are read from the dump's `answers` rows, and the
truth from its `tasks` rows, so it covers tasks that have not settled.
"""
import argparse
import json
from collections import Counter, defaultdict

OUTCOME = {"a": "yes", "b": "no", "unsure": "unsure", "disputed": "disputed"}
ANSWER = {"a": "yes", "b": "no", "unsure": "unsure"}


def read_jsonl(path: str) -> list:
    with open(path, encoding="utf-8") as f:
        return [json.loads(line) for line in f if line.strip()]


def segment_rows(export: list, set_name=None) -> tuple:
    """(rows for the settled gutter tasks, counts). Tasks of other kinds are ignored, a held or withdrawn one is counted apart."""
    rows, c = [], Counter()
    for r in export:
        if r.get("kind") != "gutter" or (set_name is not None and r.get("set") != set_name):
            continue
        c["tasks"] += 1
        if r.get("outcome") == "bad_crop":
            c["left_out_build_defect"] += 1
            continue
        if r.get("held") or r.get("outcome_withdrawn"):
            c["held_or_withdrawn"] += 1
            continue
        if not r.get("settled"):
            c["unsettled"] += 1
            continue
        out = OUTCOME.get(r.get("outcome"))
        if out is None:
            c["other_outcome"] += 1
            continue
        c["settled"] += 1
        c[out] += 1
        row = {"id": r["id"], "set": r.get("set"), "segment_id": r.get("segment_id"), "page_id": r.get("page_id"),
               "method": r.get("method"), "role": r.get("role"), "line": r.get("line"),
               "counts": {"yes": r["counts"]["a"], "no": r["counts"]["b"], "unsure": r["counts"]["unsure"],
                          "badcrop": r["counts"].get("badcrop", 0)}, "outcome": out}
        if r.get("truth") is not None:
            row["truth"] = r["truth"]
        rows.append(row)
    return rows, c


def gold_accuracy(dump: list, set_name=None) -> dict:
    """Per volunteer: answers on calibration items (tasks with a private truth) that matched it, did not, or were can't tell."""
    truth = {}
    for d in dump:
        if d.get("table") != "tasks":
            continue
        row = d["row"]
        if set_name is not None and row.get("set_name") != set_name:
            continue
        body = json.loads(row["body"])
        if body.get("kind") == "gutter" and (body.get("private") or {}).get("truth") in ("yes", "no"):
            truth[row["id"]] = body["private"]["truth"]
    per = defaultdict(Counter)
    for d in dump:
        if d.get("table") != "answers" or d["row"]["task_id"] not in truth:
            continue
        a = d["row"]
        said = ANSWER.get(a["choice"])
        if said is None:
            continue
        who = a["contributor"][:8]
        per[who]["answers"] += 1
        per[who]["unsure" if said == "unsure" else "matched" if said == truth[a["task_id"]] else "missed"] += 1
    out = {}
    for who, c in sorted(per.items()):
        decided = c["matched"] + c["missed"]
        out[who] = {"answers": c["answers"], "matched": c["matched"], "missed": c["missed"], "unsure": c["unsure"],
                    "accuracy": round(c["matched"] / decided, 4) if decided else None}
    return {"calibration_tasks": len(truth), "volunteers": out}


def main(argv=None) -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--export", required=True)
    ap.add_argument("--set", help="only this set's tasks")
    ap.add_argument("--out", help="write the per-segment rows here, as JSON lines")
    ap.add_argument("--dump", help="an /admin/dump file, for each volunteer's accuracy on calibration items")
    a = ap.parse_args(argv)
    rows, c = segment_rows(read_jsonl(a.export), a.set)
    if a.out:
        with open(a.out, "w", encoding="utf-8") as f:
            for r in sorted(rows, key=lambda r: r["id"]):
                f.write(json.dumps(r, ensure_ascii=False) + "\n")
    summary = {"tasks": c["tasks"], "unsettled": c["unsettled"], "held_or_withdrawn": c["held_or_withdrawn"],
               "left_out_build_defect": c["left_out_build_defect"],
               "settled": c["settled"], "decided": c["yes"] + c["no"], "yes": c["yes"], "no": c["no"],
               "unsure_or_disputed": {"unsure": c["unsure"], "disputed": c["disputed"], "total": c["unsure"] + c["disputed"]}}
    if a.dump:
        summary["gold"] = gold_accuracy(read_jsonl(a.dump), a.set)
    print(json.dumps(summary, indent=1, ensure_ascii=False))


if __name__ == "__main__":
    main()
