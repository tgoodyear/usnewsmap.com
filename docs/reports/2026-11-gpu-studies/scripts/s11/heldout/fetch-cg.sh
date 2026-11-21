#!/usr/bin/env bash
# Fetch Chronicling Germany split files and the pages of one split (Test by default).
# Usage: fetch-cg.sh --dest DIR [--split Test]
set -euo pipefail
dest=""; split="Test"
while [ $# -gt 0 ]; do case "$1" in --dest) dest="$2"; shift 2;; --split) split="$2"; shift 2;; *) echo "unknown flag $1" >&2; exit 2;; esac; done
[ -n "$dest" ] || { echo "--dest required" >&2; exit 2; }
B="https://gitlab.uni-bonn.de/api/v4/projects/digital-history%2FChronicling-Germany-Dataset/repository"
mkdir -p "$dest/meta" "$dest/images" "$dest/annotations"
for f in readme.md LICENSE data/split.json data/split.csv; do
  curl -fsS -o "$dest/meta/$(basename $f)" "$B/files/$(printf %s "$f" | sed "s#/#%2F#g")/raw?ref=main"
done
curl -fsS "$B/commits/main" > "$dest/meta/commit.json"
for d in images annotations; do
  : > "$dest/meta/tree-$d.json.lines"
  for p in $(seq 1 9); do curl -fsS "$B/tree?path=data/$d&per_page=100&page=$p" >> "$dest/meta/tree-$d.json.lines"; echo >> "$dest/meta/tree-$d.json.lines"; done
done
python3 -I - "$dest" "$split" > "$dest/meta/fetch-list.tsv" <<"PY"
import json,sys
dest,split=sys.argv[1],sys.argv[2]
stems=set(json.load(open(dest+"/meta/split.json"))[split])
for d in ("images","annotations"):
    for line in open(f"{dest}/meta/tree-{d}.json.lines"):
        line=line.strip()
        if not line: continue
        for e in json.loads(line):
            stem=e["name"].rsplit(".",1)[0]
            if stem in stems and e["type"]=="blob": print(d+"\t"+e["path"]+"\t"+e["name"])
PY
wc -l < "$dest/meta/fetch-list.tsv"
while IFS=$(printf "\t") read -r d path name; do
  enc=$(printf %s "$path" | python3 -I -c "import sys,urllib.parse;print(urllib.parse.quote(sys.stdin.read(),safe=\"\"))")
  [ -s "$dest/$d/$name" ] || curl -fsS --retry 3 -o "$dest/$d/$name" "$B/files/$enc/raw?ref=main&lfs=true"
done < "$dest/meta/fetch-list.tsv"
echo FETCH-CG-DONE
