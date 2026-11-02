"""Engine outputs to JSONL for gpu-study: --kind jochre (zips with <id>.txt) or tesseract (txt/<id>.txt).
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
    for f in sorted(os.listdir(os.path.join(a.dir, "txt"))):
        if f.endswith(".txt"):
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
