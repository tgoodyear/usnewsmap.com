"""Turn a glyph-check data file (the #1620 tap page's data.json) into check.usnewsmap.com tasks.

Writes OUT_DIR/clips/<lang>/<set>/... (JPEG/PNG files, served at /clips/<same path>), OUT_DIR/tasks.jsonl
for POST /admin/tasks, and OUT_DIR/language.json for PUT /admin/languages/<lang>. Standard library only.

  python3 from_tap_data.py --data data.json --lang chr --set phoenix-nt-v1 --language-config chr.json --out-dir OUT
"""
import argparse
import base64
import collections
import datetime
import json
import os

MONTHS = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"]


def save(uri: str, path: str) -> str:
    head, data = uri.split(",", 1)
    ext = "png" if "image/png" in head else "jpg"
    path = f"{path}.{ext}"
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as f:
        f.write(base64.b64decode(data))
    return path


def label(page: str, title: str) -> str:
    """`sn83020866/1828-07-23/ed-1/seq-4` -> `Cherokee Phoenix, 23 July 1828, page 4`."""
    _, date, _, seq = page.split("/")
    d = datetime.date.fromisoformat(date)
    return f"{title}, {d.day} {MONTHS[d.month - 1]} {d.year}, page {seq.split('-')[1]}"


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--data", required=True)
    ap.add_argument("--lang", required=True)
    ap.add_argument("--set", required=True)
    ap.add_argument("--language-config", required=True, help="name, intro, notes, title, drop_examples")
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--examples-set", help="where the example pictures go (default: --set); a new name keeps cached old pictures from being served")
    a = ap.parse_args()

    data = json.load(open(a.data, encoding="utf-8"))
    os.makedirs(a.out_dir, exist_ok=True)
    cfg = json.load(open(a.language_config, encoding="utf-8"))
    root = os.path.join(a.out_dir, "clips")
    prefix = f"{a.lang}/{a.set}"
    rel = lambda p: os.path.relpath(p, root)

    n = 0
    with open(os.path.join(a.out_dir, "tasks.jsonl"), "w", encoding="utf-8") as out:
        for line in data["lines"]:
            stem = line["crop"].rsplit(".", 1)[0]
            line_key = rel(save(line["line"], os.path.join(root, prefix, "lines", stem)))
            for d in line["decisions"]:
                word_key = rel(save(d["word"], os.path.join(root, prefix, "words", d["id"].replace("~", "_w"))))
                out.write(json.dumps({
                    "id": f"{prefix}/{d['id']}", "lang": a.lang, "set": a.set,
                    "page": line["page"], "source": f"https://chroniclingamerica.loc.gov/lccn/{line['page']}/",
                    "label": label(line["page"], cfg["title"]),
                    "line": line_key, "word": word_key, "box": d["box"],
                    "a": d["a"], "b": d["b"], "a_diff": d["a_diff"], "b_diff": d["b_diff"],
                    "a_is": "reference", "b_is": "loc",
                }, ensure_ascii=False) + "\n")
                n += 1

    # Example clips of each letter, except letters whose clips were found to be wrong.
    drop = set(cfg.get("drop_examples", ""))
    examples = collections.defaultdict(list)
    for ch, uris in data["examples"].items():
        if ch in drop or len(uris) < cfg.get("min_examples", 3):
            continue
        for i, u in enumerate(uris[:3]):
            examples[ch].append(rel(save(u, os.path.join(root, a.lang, a.examples_set or a.set, "examples", f"{ord(ch):04x}_{i}"))))
    language = {k: v for k, v in cfg.items() if k not in ("drop_examples", "min_examples")}
    language["examples"] = examples
    json.dump(language, open(os.path.join(a.out_dir, "language.json"), "w", encoding="utf-8"), ensure_ascii=False, indent=1)
    print(f"{n} tasks, {len(examples)} letters with examples")


if __name__ == "__main__":
    main()
