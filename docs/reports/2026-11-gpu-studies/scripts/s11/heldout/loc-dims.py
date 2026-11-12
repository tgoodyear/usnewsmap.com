"""Print LoC JP2 size and URLs for CA page ids (lccn_date_ed-N_seq-M). Usage: --ids ID[,ID...] --out FILE"""
import argparse, json, time, urllib.request
ap = argparse.ArgumentParser(); ap.add_argument("--ids", required=True); ap.add_argument("--out", required=True); a = ap.parse_args()
UA = {"User-Agent": "usnewsmap-ocr-survey/1.0 (+https://usnewsmap.com)"}
rows = []
for pid in a.ids.split(","):
    lccn, date, ed, seq = pid.split("_"); seq = int(seq.split("-")[1])
    url = f"https://www.loc.gov/item/{lccn}/{date}/{ed}/?fo=json"
    try:
        d = json.load(urllib.request.urlopen(urllib.request.Request(url, headers=UA), timeout=60))
        files = d["resources"][0]["files"][seq - 1]
        jp2 = next(f for f in files if f.get("mimetype") == "image/jp2")
        alto = next((f["url"] for f in files if f.get("mimetype") == "text/xml"), None)
        row = {"id": pid, "w": jp2["width"], "h": jp2["height"], "jp2": jp2["url"], "alto": alto}
    except Exception as e:
        row = {"id": pid, "error": str(e)}
    rows.append(row); print(json.dumps(row), flush=True); time.sleep(1.5)
json.dump(rows, open(a.out, "w"), indent=1)
