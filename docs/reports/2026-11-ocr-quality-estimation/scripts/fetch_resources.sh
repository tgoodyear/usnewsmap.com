#!/usr/bin/env bash
# Downloads the open lexica used by the quality-estimation study into --out (the lake's
# qe-resources directory). Every file is kept, with its source URL and sha256 in SOURCES.tsv.
#
#   fetch_resources.sh --out /srv/usnm-lake/qe-resources
set -euo pipefail
OUT=""
while [ $# -gt 0 ]; do case "$1" in --out) OUT="$2"; shift 2;; *) echo "unknown flag $1" >&2; exit 2;; esac; done
[ -n "$OUT" ] || { echo "--out is required" >&2; exit 2; }
mkdir -p "$OUT/hunspell" "$OUT/wheels"
LO_SHA=$(curl -fsS https://api.github.com/repos/LibreOffice/dictionaries/commits/master | python3 -c 'import json,sys; print(json.load(sys.stdin)["sha"])')
echo "LibreOffice/dictionaries at $LO_SHA"
FILES="
cs_CZ/cs_CZ.aff cs_CZ/cs_CZ.dic cs_CZ/README_en.txt
pl_PL/pl_PL.aff pl_PL/pl_PL.dic pl_PL/README_en.txt pl_PL/README_pl_PL.txt
ru_RU/ru_RU.aff ru_RU/ru_RU.dic ru_RU/README_ru_RU.txt
sr/sr.aff sr/sr.dic sr/sr-Latn.aff sr/sr-Latn.dic sr/README.txt
es/es_ES.aff es/es_ES.dic es/LICENSE.md es/README_hunspell_es.txt
fr_FR/dictionaries/fr.aff fr_FR/dictionaries/fr.dic fr_FR/dictionaries/README_dict_fr.txt
de/de_DE_frami.aff de/de_DE_frami.dic de/README_de_DE_frami.txt
en/en_US.aff en/en_US.dic en/README_en_US.txt
sv_SE/dictionaries/sv_SE.aff sv_SE/dictionaries/sv_SE.dic sv_SE/LICENSE_en_US.txt
sl_SI/sl_SI.aff sl_SI/sl_SI.dic sl_SI/README_sl_SI.txt
no/nb_NO.aff no/nb_NO.dic no/README_NO.txt
da_DK/da_DK.aff da_DK/da_DK.dic da_DK/README_da_DK.txt
"
: > "$OUT/SOURCES.tsv.new"
for f in $FILES; do
  url="https://raw.githubusercontent.com/LibreOffice/dictionaries/$LO_SHA/$f"
  dest="$OUT/hunspell/$f"
  mkdir -p "$(dirname "$dest")"
  [ -s "$dest" ] || curl -fsS -o "$dest" "$url"
  printf '%s\t%s\t%s\n' "hunspell/$f" "$url" "$(sha256sum "$dest" | cut -d' ' -f1)" >> "$OUT/SOURCES.tsv.new"
done
# wordfreq 3.1.1 (code Apache-2.0, data CC BY-SA 4.0): the wheel carries the frequency lists.
python3 -m pip download --no-deps --only-binary :all: -d "$OUT/wheels" wordfreq==3.1.1 >/dev/null
for w in "$OUT"/wheels/*.whl; do printf '%s\t%s\t%s\n' "wheels/$(basename "$w")" "pypi:wordfreq==3.1.1" "$(sha256sum "$w" | cut -d' ' -f1)" >> "$OUT/SOURCES.tsv.new"; done
mv "$OUT/SOURCES.tsv.new" "$OUT/SOURCES.tsv"
chmod -R a+rX "$OUT"
