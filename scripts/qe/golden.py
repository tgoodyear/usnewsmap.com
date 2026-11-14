"""Golden vectors for usnm_core::qe from the study's own qe_lib (run with the study's scripts dir on sys.path)."""
import json, sys
import argparse
ap = argparse.ArgumentParser()
ap.add_argument("--study-scripts", required=True, help="the study's scripts directory, with qe_lib.py")
sys.path.insert(0, ap.parse_args().study_scripts)
from qe_lib import CharLM, fold, words, lm_text, norm_text

train = ("the mayor of albany spoke with the council about bridges and roads across the river. "
         "a new bridge was opened on the third of may, 0000, by the governor; the people cheered. ") * 3
lm = CharLM(5); lm.train(train)
lex = sorted({fold(w, "eng") for w in words(train)} | {"don't", "co", "op"})
texts = [
    "The Mayor of Albany spoke with the counci1 about bridges",
    "tbe rnayor of A1bany spokc wiht tlie coimcil",
    "co-\n  operation and the gover-\nnor's men; don't",
    "ſtrange Things, 1903 & ½ — été Café",
    "[?]",
    "",
    "   \n\t ",
    "x\u001cy  z w",
    "a-\nb-\nc",
    "αβγ ΣΟΦΟΣ",
]
cases = []
for t in texts:
    w = [fold(x, "eng") for x in words(t)]
    lt = lm_text(t, "eng")
    lp, nc = lm.logprob(lt) if lt else (0.0, 0)
    n = len(w); known = sum(x in lex for x in w)
    cases.append({"text": t, "normalized": norm_text(t), "words": w, "lm_text": lt, "logprob": lp, "chars": nc,
                  "known": known, "known3": sum(1 for x in w if len(x) >= 3 and x in lex),
                  "oov": (1 - known / n) if n else None, "bpc": (-lp / nc) if nc else None})
folds = []
for lang, t in [("rus", "Новый мiръ ѣсть ѲЕДОРЪ объявленiе"), ("srp", "Ђорђе Љубић ЏЕП Њива ѣ"), ("yid", "אָבער װײַט ױ ײ"),
                ("deu", "Straße ſoll ẞ"), ("srp", "Beograd je grad"), ("rus", "ъ ъъ съезд")]:
    folds.append({"lang": lang, "text": t, "fold": fold(t, lang), "words": [fold(x, lang) for x in words(t)], "lm_text": lm_text(t, lang)})
counts = sorted(set(lm.cnt) | set(lm.ctx))
json.dump({"train": train, "vocab": lm.V, "lexicon": lex,
           "entries": [[k, lm.cnt.get(k, 0), lm.ctx.get(k, 0), lm.types.get(k, 0)] for k in counts],
           "cases": cases, "folds": folds}, sys.stdout, ensure_ascii=False, indent=0)
