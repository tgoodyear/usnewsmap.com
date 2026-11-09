"""Cherokee volume and LoC word-confidence stats from LoC OCR batch tars (ALTO). Usage: --tar PATH [--tar PATH]"""
import argparse, collections, json, re, statistics, tarfile, xml.etree.ElementTree as ET
CHR = re.compile(r"[Ꭰ-Ᏽꭰ-ꮿ]")
ap = argparse.ArgumentParser(); ap.add_argument("--tar", action="append", required=True); a = ap.parse_args()
pages = 0; pages_with_chr = 0; blocks = collections.Counter(); words = collections.Counter(); chars = collections.Counter()
wc = collections.defaultdict(list); lines = collections.Counter(); per_page_chr = []; langs = collections.Counter()
for t in a.tar:
    with tarfile.open(t, "r:bz2") as tf:
        for m in tf:
            if not m.name.endswith(".xml"): continue
            pages += 1
            root = ET.parse(tf.extractfile(m)).getroot()
            n_chr = 0
            for el in root.iter():
                if not el.tag.endswith("TextBlock"): continue
                lang = el.get("language") or "none"; langs[lang] += 1
                for ln in el.iter():
                    if ln.tag.endswith("TextLine"): lines[lang] += 1
                    if not ln.tag.endswith("String"): continue
                    s = ln.get("CONTENT") or ""
                    script = "chr" if CHR.search(s) else ("lat" if re.search(r"[A-Za-z]", s) else "other")
                    words[(lang, script)] += 1
                    for ch in s:  # each character by its own script, so "9Ꮞ." adds one syllabary character
                        chars["chr" if CHR.match(ch) else ("lat" if ch.isascii() and ch.isalpha() else "other")] += 1
                    if script == "chr": n_chr += 1
                    if ln.get("WC") is not None: wc[(lang, script)].append(float(ln.get("WC")))
            per_page_chr.append(n_chr); pages_with_chr += n_chr > 0
def q(v):
    v = sorted(v); n = len(v)
    return {"n": n, "mean": round(statistics.fmean(v), 3), "p10": v[n//10], "median": v[n//2], "p90": v[9*n//10],
            "share_lt_0.5": round(sum(x < 0.5 for x in v)/n, 3)} if v else None
print(json.dumps({"pages": pages, "pages_with_chr_words": pages_with_chr,
    "pages_chr_ge_100_words": sum(x >= 100 for x in per_page_chr), "block_languages": langs, "lines_by_block_lang": lines,
    "strings_by_lang_script": {f"{k[0]}/{k[1]}": v for k, v in words.items()},
    "chars_by_script": chars, "word_confidence": {f"{k[0]}/{k[1]}": q(v) for k, v in wc.items()},
    "chr_words_per_page": q(per_page_chr)}, ensure_ascii=False, indent=1))
