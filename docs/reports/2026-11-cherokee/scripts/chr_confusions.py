"""Letter confusions: Tesseract vs NT reference, and LoC vs Tesseract on the same Phoenix Cherokee lines.
Usage: --eval-dir DIR"""
import argparse, collections, difflib, glob, json, os, re
ap = argparse.ArgumentParser(); ap.add_argument("--eval-dir", required=True); a = ap.parse_args()
CH = re.compile(r"[Ꭰ-Ᏽ]")
def chr_only(s): return "".join(c for c in s if CH.match(c))
def subs(ref, hyp, counter):
    sm = difflib.SequenceMatcher(None, ref, hyp, autojunk=False)
    for op, i1, i2, j1, j2 in sm.get_opcodes():
        if op == "replace" and i2 - i1 == j2 - j1:
            for x, y in zip(ref[i1:i2], hyp[j1:j2]): counter[(x, y)] += 1
out = {}
nt = os.path.join(a.eval_dir, "nt-reference/nt-engine-sample")
for model in ("best", "fast"):
    c = collections.Counter(); total = 0
    for ref in glob.glob(f"{nt}/refs/*.txt"):
        hyp = f"{nt}/out/{model}/{os.path.basename(ref)}"
        if not os.path.exists(hyp): continue
        r, h = chr_only(open(ref).read()), chr_only(open(hyp).read()); total += len(r); subs(r, h, c)
    out[f"nt_{model}_top_substitutions"] = [f"{x}->{y} {n}" for (x, y), n in c.most_common(12)]
    out[f"nt_{model}_na_to_wa"] = c[("Ꮎ", "Ꮹ")]; out[f"nt_{model}_ref_na"] = None
fam = ["ᎾᏍᎩ", "ᏩᏍᎩ", "ᏣᏍᎩ", "ᏳᏍᎩ"]
loc_tok = collections.Counter(); tes_tok = collections.Counter(); c = collections.Counter(); n_lines = 0
for f in glob.glob(os.path.join(a.eval_dir, "region-aware/out/hybrid/*.json")):
    for b in json.load(open(f))["blocks"]:
        for ln in b["lines"]:
            if ln.get("tesseract_text") is None: continue
            n_lines += 1
            s, t = ln["source_text"] or "", ln["tesseract_text"] or ""
            loc_tok.update(re.findall(r"[Ꭰ-Ᏽ]+", s)); tes_tok.update(re.findall(r"[Ꭰ-Ᏽ]+", t))
            subs(chr_only(s), chr_only(t), c)
out["phoenix_lines_compared"] = n_lines
out["phoenix_nasgi_family_loc"] = {w: loc_tok[w] for w in fam}
out["phoenix_nasgi_family_tesseract_best"] = {w: tes_tok[w] for w in fam}
out["phoenix_loc_to_tesseract_top_substitutions"] = [f"{x}->{y} {n}" for (x, y), n in c.most_common(15)]
print(json.dumps(out, ensure_ascii=False, indent=1))
