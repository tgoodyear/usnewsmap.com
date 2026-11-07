"""Masthead recall proxy: does each issue's page 1 OCR contain the nameplate ᏣᎳᎩ ᏧᎴᎯᏌᏅᎯ? Usage: --tar PATH ..."""
import argparse, collections, re, tarfile, json
ap = argparse.ArgumentParser(); ap.add_argument("--tar", action="append", required=True); a = ap.parse_args()
res = collections.Counter(); words = collections.Counter(); miss = []
for t in a.tar:
    with tarfile.open(t, "r:bz2") as tf:
        for m in tf:
            if not m.name.endswith("ocr.txt"): continue
            txt = tf.extractfile(m).read().decode("utf-8", "replace")
            toks = re.findall(r"[Ꭰ-Ᏽ]+", txt)
            words.update(toks)
            if "/seq-1/" in m.name:
                res["front_pages"] += 1
                if "ᏣᎳᎩ" in toks and "ᏧᎴᎯᏌᏅᎯ" in toks: res["both_exact"] += 1
                elif "ᏣᎳᎩ" in toks: res["cherokee_only"] += 1
                else: miss.append(m.name)
            if "ᏣᎳᎩ" in txt and "ᏧᎴᎯᏌᏅᎯ" in txt: res["pages_with_nameplate_anywhere"] += 1
print(json.dumps({"masthead": res, "front_pages_missing_both": len(miss),
  "nasgi_family": {w: words[w] for w in ["ᎾᏍᎩ","ᏩᏍᎩ","ᏣᏍᎩ","ᏳᏍᎩ","ᎾᏍᎩᏃ","ᏩᏍᎩᏃ"]},
  "ale_family": {w: words[w] for w in ["ᎠᎴ","ᎠᏞ","ᎠᎮ","ᎤᎴ"]},
  "distinct_chr_tokens": len(words), "chr_tokens": sum(words.values())}, ensure_ascii=False, indent=1))
