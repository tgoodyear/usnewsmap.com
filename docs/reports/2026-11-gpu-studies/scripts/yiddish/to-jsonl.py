"""Engine outputs to JSONL: Jochre zips or Tesseract text with a successful latest timing receipt.
--dir DIR, --key page|id (the JSON field the file stem goes in), --out FILE, --idmap (stem a_b -> a:b for crop ids)."""
import argparse, json, os, zipfile
ap = argparse.ArgumentParser()
ap.add_argument("--kind", required=True, choices=["jochre", "tesseract"])
ap.add_argument("--dir", required=True)
ap.add_argument("--key", default="page")
ap.add_argument("--idmap", action="store_true")
ap.add_argument("--out", required=True)
a = ap.parse_args()
rows = []
if a.kind == "jochre":
    for f in sorted(os.listdir(os.path.join(a.dir, "zip"))):
        stem = f[:-4]
        try:
            z = zipfile.ZipFile(os.path.join(a.dir, "zip", f))
            names = [n for n in z.namelist() if n.endswith(".txt")]
            text = z.read(names[0]).decode("utf-8") if names else None
        except zipfile.BadZipFile:
            text = None
        rows.append((stem, text))
else:
    receipts = {}
    try:
        with open(os.path.join(a.dir, "timing.tsv"), encoding="utf-8") as timing:
            for line in timing:
                parts = line.rstrip("\n").split("\t")
                if parts[0]:
                    receipts[parts[0]] = len(parts) == 3 and parts[2] == "0"
    except FileNotFoundError:
        pass
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
