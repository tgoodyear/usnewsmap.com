#!/usr/bin/env python3 -I
"""Compare macOS page outputs with the Linux native (4d6e) and container (dac9) outputs, and the gold challenge.
usage: analyze.py --root DIR --out DIR"""
import argparse, glob, hashlib, json, os, re, tarfile
import xml.etree.ElementTree as ET
ap = argparse.ArgumentParser(); ap.add_argument("--root", required=True); ap.add_argument("--out", required=True)
ap.add_argument("--arch", default="arm64"); a = ap.parse_args()
R = a.root; REF = R + "/linux-ref"; RUN = R + "/run-" + a.arch
sha = lambda p: hashlib.sha256(open(p, "rb").read()).hexdigest()

def strings(p):
    t = ET.parse(p); out = []
    for e in t.iter():
        if e.tag.split("}")[-1] == "String": out.append(dict(e.attrib))
    return out
def strip_ns(tag): return tag.split("}")[-1]
def meta(p):  # everything outside the Layout element, as a string
    s = open(p, encoding="utf-8").read(); i = s.find("<Layout")
    return s[:i] if i >= 0 else s
def classify(mac, lin):
    r = {"identical_bytes": sha(mac) == sha(lin)}
    if r["identical_bytes"]: return r
    ms, ls = strings(mac), strings(lin)
    r["strings_mac"], r["strings_linux"] = len(ms), len(ls)
    r["metadata_differs"] = meta(mac) != meta(lin)
    if len(ms) == len(ls):
        c = {"text": 0, "coordinates": 0, "confidence": 0, "other_attr": 0}
        for x, y in zip(ms, ls):
            if x.get("CONTENT") != y.get("CONTENT"): c["text"] += 1
            if any(x.get(k) != y.get(k) for k in ("HPOS", "VPOS", "WIDTH", "HEIGHT")): c["coordinates"] += 1
            if x.get("WC") != y.get("WC"): c["confidence"] += 1
            if any(x.get(k) != y.get(k) for k in set(x) | set(y) if k not in ("CONTENT", "HPOS", "VPOS", "WIDTH", "HEIGHT", "WC")): c["other_attr"] += 1
        r["strings_differing_by_class"] = c
    else:
        import difflib
        wm, wl = [s.get("CONTENT") for s in ms], [s.get("CONTENT") for s in ls]
        sm = difflib.SequenceMatcher(None, wm, wl, autojunk=False)
        r["word_alignment_ratio"] = round(sm.ratio(), 5)
        r["word_ops"] = {t: sum(max(i2 - i1, j2 - j1) for tag, i1, i2, j1, j2 in sm.get_opcodes() if tag == t) for t in ("replace", "delete", "insert")}
        eq = [(i1, j1, i2 - i1) for tag, i1, i2, j1, j2 in sm.get_opcodes() if tag == "equal"]
        cc = 0
        for i1, j1, n in eq:
            for k in range(n):
                x, y = ms[i1 + k], ls[j1 + k]
                if any(x.get(q) != y.get(q) for q in ("HPOS", "VPOS", "WIDTH", "HEIGHT")): cc += 1
        r["coordinate_diffs_among_matched_words"] = cc
    return r
rows = []
for d in sorted(glob.glob(RUN + "/pages/*/")):
    n = os.path.basename(d.rstrip("/"))
    if not os.path.exists(d + "text.txt"): rows.append({"page": n, "status": "no output"}); continue
    row = {"page": n.replace("tank_ocr_scans_keep_nn_", ""), "format": "jp2"}
    for k, ldir in (("vs_linux_native", REF + "/spot/runs/" + n + "/native"), ("vs_linux_container", REF + "/spot/runs/" + n + "/container")):
        t = {"text_identical": sha(d + "text.txt") == sha(ldir + "/text.txt"), "alto_identical": sha(d + "layout.xml") == sha(ldir + "/layout.xml")}
        t["text_bytes"] = [os.path.getsize(d + "text.txt"), os.path.getsize(ldir + "/text.txt")]
        t["alto"] = classify(d + "layout.xml", ldir + "/layout.xml")
        if not t["text_identical"]:
            import difflib
            m, l = open(d + "text.txt", encoding="utf-8").read(), open(ldir + "/text.txt", encoding="utf-8").read()
            sm = difflib.SequenceMatcher(None, m.split(), l.split(), autojunk=False)
            t["text_word_ratio"] = round(sm.ratio(), 5)
        row[k] = t
    ml = json.load(open(d + "result.json")); ll = json.load(open(REF + "/spot/runs/" + n + "/native/result.json"))
    row["text_sha256"] = {"macos": ml["text_sha256"], "linux_native": ll["text_sha256"]}
    row["elapsed_ms"] = {"macos": ml["elapsed_ms"], "linux_native": ll["elapsed_ms"]}
    rows.append(row)
jrows = []

import difflib
for i in sorted(os.listdir(RUN + "/jpeg")):
    d = RUN + "/jpeg/" + i + "/"
    if not os.path.isdir(d): continue
    ldir = REF.replace("linux-ref", "jpeg-ref") + "/" + i + "/linux"
    t = {"text_identical": sha(d + "text.txt") == sha(ldir + "/text.txt"), "alto_identical": sha(d + "layout.xml") == sha(ldir + "/layout.xml")}
    t["alto"] = classify(d + "layout.xml", ldir + "/layout.xml")
    if not t["text_identical"]:
        sm = difflib.SequenceMatcher(None, open(d + "text.txt", encoding="utf-8").read().split(), open(ldir + "/text.txt", encoding="utf-8").read().split(), autojunk=False)
        t["text_word_ratio"] = round(sm.ratio(), 5)
    jrows.append({"page": "s9-en-asgold-" + i + ".jpg", "format": "jpeg", "vs_linux_native": t})
def agg(k): ok = [r for r in rows + jrows if r.get(k)]; return {"pages": len(ok), "text_identical": sum(r[k]["text_identical"] for r in ok), "alto_identical": sum(r[k]["alto_identical"] for r in ok)}
rows_all = rows + jrows
pages = {"pages": rows_all, "summary": {"vs_linux_native": agg("vs_linux_native"), "vs_linux_container": agg("vs_linux_container")}}
# gold
m = json.load(open(RUN + "/gold/gold-result.json"))
runs = {"linux_container_247c": "run-container-247c", "linux_native_c353": "run-native-c353", "linux_native_4d6e": "run-native-4d6e"}
L = {k: json.load(open(REF + "/" + v + "/slot/gold-result.json")) for k, v in runs.items()}
member = {c["crop_id"]: c["image"]["member"] for c in json.loads(tarfile.open(REF + "/campaign/bundle.tar").extractfile(
    [n for n in tarfile.open(REF + "/campaign/bundle.tar").getnames() if n.endswith("manifest.json")][0]).read())["crops"]}
mc = {c["crop_id"]: c for c in m["crops"]}
crops = []
for cid in sorted(mc):
    c = mc[cid]; row = {"crop": cid, "image": member[cid].rsplit(".", 1)[-1], "ref_chars": c["ref_chars"], "macos_char_edits": c["char_edits"], "macos_word_edits": c["word_edits"],
                         "macos_cer": round(c["char_edits"] / c["ref_chars"], 4)}
    for k, r in L.items():
        x = {y["crop_id"]: y for y in r["crops"]}[cid]; row[k + "_char_edits"] = x["char_edits"]; row[k + "_word_edits"] = x["word_edits"]
    row["linux_cer"] = round(row["linux_native_4d6e_char_edits"] / c["ref_chars"], 4)
    row["identical_to_linux"] = (row["macos_char_edits"], row["macos_word_edits"]) == (row["linux_native_4d6e_char_edits"], row["linux_native_4d6e_word_edits"])
    crops.append(row)
gold = {"macos_summary": m["summary"][0], "linux_summaries": {k: r["summary"][0] for k, r in L.items()}, "threshold": 0.08734328574534386,
        "manifest_sha256": m["manifest_sha256"], "macos_engine": m["boinc"],
        "crops_identical_to_linux": sum(r["identical_to_linux"] for r in crops), "crops": len(crops),
        "linux_container_vs_linux_native_crops_differing": sum((x["char_edits"], x["word_edits"]) != (y["char_edits"], y["word_edits"]) for x, y in zip(L["linux_container_247c"]["crops"], L["linux_native_4d6e"]["crops"])),
        "by_image_type": {}, "per_crop": crops}
for t in ("jpg", "tif"):
    s = [r for r in crops if r["image"] == t]
    gold["by_image_type"][t] = {"crops": len(s), "identical": sum(r["identical_to_linux"] for r in s), "mac_edits": sum(r["macos_char_edits"] for r in s),
                                "linux_edits": sum(r["linux_native_4d6e_char_edits"] for r in s), "ref_chars": sum(r["ref_chars"] for r in s)}
os.makedirs(a.out, exist_ok=True)
json.dump(pages, open(a.out + "/parity-pages.json", "w"), indent=1, sort_keys=True)
json.dump(gold, open(a.out + "/challenge-gold.json", "w"), indent=1, sort_keys=True)
print(json.dumps(pages["summary"])); print(json.dumps({k: v for k, v in gold.items() if k != "per_crop"}, indent=1))
