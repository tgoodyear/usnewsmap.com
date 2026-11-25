"""Lists the tasks check.usnewsmap.com closed as `bad_crop`, for rebuilding (README.md, "Bad crop").

  curl -s 'https://check.usnewsmap.com/api/export?lang=chr-lines' > check-chr-lines.jsonl
  python3 bad_crops.py check-chr-lines.jsonl [more exports ...] [--json]

A task settles as `bad_crop` when two thirds of its answers (at least 3) say Bad crop: the box shows more than one line, cuts
off the word or marks the wrong thing. It is a build defect, not reader evidence, and is held with hold_reason `bad_crop`.
For each one this prints the task id, its set and page, and the clip keys it was built with (`clips` in the export: a pick
task's `line` and `word`, a gutter task's `crop`), with its answer counts. --json writes one JSON object per task instead.
"""
import argparse
import json


def bad_crops(rows: list) -> list:
    out = []
    for r in rows:
        if r.get("outcome") != "bad_crop":
            continue
        out.append({"id": r["id"], "set": r.get("set"), "page": r.get("page"), "kind": r.get("kind", "pick"),
                    "clips": r.get("clips") or {}, "counts": r.get("counts") or {}})
    return sorted(out, key=lambda t: t["id"])


def main(argv=None) -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("exports", nargs="+", help="/api/export files (JSON lines)")
    ap.add_argument("--json", action="store_true", help="one JSON object per task")
    a = ap.parse_args(argv)
    rows = []
    for path in a.exports:
        with open(path, encoding="utf-8") as f:
            rows += [json.loads(line) for line in f if line.strip()]
    found = bad_crops(rows)
    for t in found:
        if a.json:
            print(json.dumps(t, ensure_ascii=False))
        else:
            clips = " ".join(f"{k}={v}" for k, v in sorted(t["clips"].items()))
            print(f"{t['id']}\tset={t['set']}\tpage={t['page']}\t{clips}\tbadcrop={t['counts'].get('badcrop', 0)}/{sum(t['counts'].values())}")
    if not a.json:
        print(f"{len(found)} bad_crop task(s)")


if __name__ == "__main__":
    main()
