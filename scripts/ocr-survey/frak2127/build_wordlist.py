#!/usr/bin/env python3
"""The German word list for arm C's trusted words (#2127, amendment 1).

Two sources, each normalised with frak_norm.normalise and then lower-cased:
  - the Deutsches Textarchiv's 19th-century core corpus as plain text
    (dta_kernkorpus_1800-1899_2020-09-23_text.zip, CC BY-SA 4.0): every word that occurs at least
    DTA_MIN_COUNT times, so one-off typos and transcription slips stay out;
  - wordfreq 3.1.1's large German list (wordfreq/data/large_de.msgpack.gz from the PyPI wheel;
    the word data is CC BY-SA 4.0): every word in it.
A word is a run of letters (Unicode letters, no digits or underscore) after normalisation.

    build_wordlist.py --dta DTA_text.zip --wordfreq large_de.msgpack.gz --out wordlist.txt
"""
import argparse, collections, gzip, hashlib, json, re, sys, zipfile

import msgpack

from frak_norm import normalise

DTA_MIN_COUNT = 2
WORD = re.compile(r"[^\W\d_]+")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--dta", required=True); ap.add_argument("--wordfreq", required=True)
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    dta = collections.Counter()
    files = 0
    with zipfile.ZipFile(a.dta) as z:
        for name in z.namelist():
            if name.endswith(".txt"):
                files += 1
                dta.update(w.lower() for w in WORD.findall(normalise(z.read(name).decode("utf-8", "replace"))))
    dta_words = {w for w, c in dta.items() if c >= DTA_MIN_COUNT}
    packed = msgpack.unpackb(gzip.open(a.wordfreq).read(), raw=False)
    assert packed[0].get("format") == "cB", packed[0]
    wf = set()
    for bucket in packed[1:]:
        for w in bucket:
            wf.update(x.lower() for x in WORD.findall(normalise(w)) if x == normalise(w))
    words = sorted(dta_words | wf)
    with open(a.out, "w", encoding="utf-8") as f:
        f.write("".join(w + "\n" for w in words))
    sha = lambda p: hashlib.sha256(open(p, "rb").read()).hexdigest()
    summary = {"dta_files": files, "dta_tokens": sum(dta.values()), "dta_types": len(dta),
               "dta_min_count": DTA_MIN_COUNT, "dta_words": len(dta_words), "wordfreq_words": len(wf),
               "words": len(words), "both": len(dta_words & wf),
               "sha256": {"dta_zip": sha(a.dta), "wordfreq": sha(a.wordfreq), "wordlist": sha(a.out)}}
    json.dump(summary, open(a.out + ".summary.json", "w"), indent=1)
    print(json.dumps(summary, indent=1))


if __name__ == "__main__":
    sys.exit(main())
