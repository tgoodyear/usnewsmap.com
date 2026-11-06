"""Engine outputs to JSONL: valid Jochre zips or Tesseract text with a successful latest timing receipt.
--dir DIR, --key page|id (the JSON field the file stem goes in), --out FILE, --idmap (stem a_b -> a:b for crop ids)."""
import argparse, json, os, struct, zipfile, zlib
ap = argparse.ArgumentParser()
ap.add_argument("--kind", required=True, choices=["jochre", "tesseract"])
ap.add_argument("--dir", required=True)
ap.add_argument("--key", default="page")
ap.add_argument("--idmap", action="store_true")
ap.add_argument("--out", required=True)
a = ap.parse_args()
rows = []
receipts = {}
try:
    with open(os.path.join(a.dir, "timing.tsv"), encoding="utf-8") as timing:
        for line in timing:
            parts = line.rstrip("\n").split("\t")
            if parts[0]:
                receipts[parts[0]] = len(parts) == 3 and parts[2] == ("200" if a.kind == "jochre" else "0")
except FileNotFoundError:
    pass
if a.kind == "jochre":
    for f in sorted(os.listdir(os.path.join(a.dir, "zip"))):
        if not f.endswith(".zip") or not receipts.get(f[:-4], False):
            continue
        stem = f[:-4]
        try:
            with zipfile.ZipFile(os.path.join(a.dir, "zip", f)) as z:
                names = [n for n in z.namelist() if n.endswith(".txt")]
                if not names or z.testzip() is not None:
                    continue
                text = z.read(names[0]).decode("utf-8")
        except (OSError, EOFError, RuntimeError, ValueError, OverflowError, zipfile.BadZipFile, struct.error, zlib.error):
            continue
        rows.append((stem, text))
else:
    for f in sorted(os.listdir(os.path.join(a.dir, "txt"))):
        if f.endswith(".txt") and receipts.get(f[:-4], False):
            rows.append((f[:-4], open(os.path.join(a.dir, "txt", f), encoding="utf-8", errors="replace").read()))
with open(a.out, "w", encoding="utf-8") as o:
    for stem, text in rows:
        k = stem
        if a.idmap:
            i = stem.rfind("_")
            k = stem[:i] + ":" + stem[i + 1:]
        r = {a.key: k}
        if text is not None:
            r["text"] = text
        o.write(json.dumps(r, ensure_ascii=False) + "\n")
print(len(rows), "rows")
