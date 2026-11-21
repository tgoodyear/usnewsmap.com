#!/usr/bin/env bash
# Polite, resumable subset downloaders for the Part A German Fraktur truth sets.
#
#   DATA_ROOT=/data ./fetch.sh --list --set chronicling # show layout, download nothing big
#   DATA_ROOT=/data ./fetch.sh --i-am-a-person --set chronicling --set reichsanzeiger
#   DATA_ROOT=/data ./fetch.sh --set nzz
#   DATA_ROOT=/data ./fetch.sh --i-am-a-person --with-austrian --set all
#
# robots.txt: gitlab.uni-bonn.de disallows /*/*.git/* (the Chronicling Germany
# clone and its LFS images) and digi.bib.uni-mannheim.de disallows
# /reichsanzeiger.fcgi (the Reichsanzeiger images). A person must run those two
# sets, so they refuse to start without --i-am-a-person. Zenodo /api is
# disallowed too: NZZ uses the direct file URL after a 10 s crawl delay.
#
# Each set lands in $DATA_ROOT/<set>/ with its LICENSE file(s) and MANIFEST.tsv
# (sha256, bytes, path). >= 1 s sleep between requests. Rerunning resumes
# (git sparse clone is updated in place; curl writes <file>.part, resumes it with
# -C - and renames only on success; finished files are skipped).
# Nothing is redistributed: CC BY-NC sets (chronicling, nzz, austrian) are for
# evaluation only.
#
# The year filter is the function year_ok below. It takes the first 4-digit
# 18xx/19xx year of the file name (Reichsanzeiger 1891_1_0001.xml is 1891;
# NZZ nzz_18910302_... is 1891),
# then of the parent directories nearest first (a range such as 1820-1939 is
# no year), then the metadata map (a TSV "page_id<TAB>year", set $CG_YEAR_MAP). Run --list first and
# adjust year_ok / the *_select functions to the layout actually seen.
set -euo pipefail

: "${DATA_ROOT:?set DATA_ROOT}"
Y0=${Y0:-1850}; Y1=${Y1:-1920}
SLEEP=${SLEEP:-1.5}                 # seconds between requests (>= 1.5)
CG_REPO=${CG_REPO:-https://gitlab.uni-bonn.de/digital-history/Chronicling-Germany-Dataset.git}
RA_REPO=${RA_REPO:-https://github.com/UB-Mannheim/reichsanzeiger-gt.git}
AT_REPO=${AT_REPO:-https://github.com/UB-Mannheim/AustrianNewspapers.git}
NZZ_RECORD=${NZZ_RECORD:-3333627}
AT_RECORD=${AT_RECORD:-3387369}
CG_YEAR_MAP=${CG_YEAR_MAP:-}
UA="german-truth-fetch/1 (research; polite)"
NZZ_ZIP=${NZZ_ZIP:-https://zenodo.org/records/3333627/files/NZZ-black-letter-ground-truth-master.zip?download=1}
NZZ_DELAY=${NZZ_DELAY:-10}          # zenodo Crawl-delay (>= 10)
AT_URLS=${AT_URLS:-}                # austrian: direct zenodo file URLs (space separated)
LIST=0; AUSTRIAN=0; PERSON=0
export GIT_LFS_SKIP_SMUDGE=1        # LFS files are pulled one at a time, politely

log() { printf '[fetch] %s\n' "$*" >&2; }
nap() { sleep "$SLEEP"; }

# --- year filter: reads paths on stdin, prints those inside Y0..Y1 --------
year_in() {  # text -> first 18xx/19xx (also the start of YYYYMMDD) not part of a range like 1820-1939
  printf '%s\n' "$1" \
    | sed -E 's/(^|[^0-9])((18|19)[0-9]{2})(0[1-9]|1[0-2])(0[1-9]|[12][0-9]|3[01])([^0-9]|$)/\1\2-\4-\5\6/; s/[0-9]{4}-[0-9]{4}//g' \
    | grep -oE '(^|[^0-9])(18[0-9]{2}|19[0-9]{2})([^0-9]|$)' | head -1 | grep -oE '[0-9]{4}' || true
}
year_of() {  # path -> year or empty: basename, then parent dirs, then metadata map
  local p=$1 y="" b d
  b=$(basename "${p%.*}"); y=$(year_in "$b")
  d=$(dirname "$p")
  while [ -z "$y" ] && [ "$d" != "." ] && [ "$d" != "/" ]; do
    y=$(year_in "$(basename "$d")"); d=$(dirname "$d")
  done
  if [ -z "$y" ] && [ -n "$CG_YEAR_MAP" ] && [ -f "$CG_YEAR_MAP" ]; then
    y=$(awk -F'\t' -v id="$b" '$1==id{print $2; exit}' "$CG_YEAR_MAP")
  fi
  printf '%s' "$y"
}
year_ok() {
  local y; y=$(year_of "$1")
  [ -n "$y" ] && [ "$y" -ge "$Y0" ] && [ "$y" -le "$Y1" ]
}

curl_get() {  # url out : polite, retry; downloads to out.part, renamed only on success
  local url=$1 out=$2
  mkdir -p "$(dirname "$out")"
  [ -s "$out" ] && return 0
  nap
  if [ -s "$out.part" ] && curl -fL --retry 5 --retry-delay 5 -C - -A "$UA" -o "$out.part" "$url"; then
    :                                   # resumed from the partial file
  else
    rm -f "$out.part"                   # none, or the server cannot resume it
    curl -fL --retry 5 --retry-delay 5 -A "$UA" -o "$out.part" "$url" || return 1
  fi
  mv "$out.part" "$out"
}

manifest() {  # dir : MANIFEST.tsv of everything except .git
  local d=$1
  ( cd "$d" && find . -type f -not -path './.git/*' -not -name MANIFEST.tsv -print0 \
      | sort -z | xargs -0 -r sha256sum \
      | awk '{h=$1; $1=""; sub(/^ +/,""); print h "\t" $0}' > MANIFEST.tsv )
  log "manifest: $(wc -l < "$d/MANIFEST.tsv") files in $d"
}

keep_licenses() {  # dir repo_dir : copy LICENSE*/COPYING*/README* into dir
  local d=$1 r=$2
  for f in "$r"/LICEN[CS]E* "$r"/COPYING* "$r"/README*; do
    [ -f "$f" ] && cp -n "$f" "$d/" || true
  done
}

sparse_clone() {  # repo dir : blobless sparse clone, top-level files only at first
  local repo=$1 dir=$2
  if [ -d "$dir/.git" ]; then
    nap; git -C "$dir" fetch --filter=blob:none origin >&2 || true
  else
    nap; git clone --filter=blob:none --sparse --no-checkout "$repo" "$dir" >&2
    git -C "$dir" sparse-checkout init --no-cone >&2
    git -C "$dir" sparse-checkout set '/LICEN*' '/COPYING*' '/README*' >&2
    git -C "$dir" checkout >&2
  fi
}

repo_files() { git -C "$1" ls-tree -r --name-only "$(git -C "$1" symbolic-ref --short HEAD 2>/dev/null || echo HEAD)" ; }

checkout_selected() {  # clonedir listfile : sparse-checkout exactly these files, politely in batches
  local dir=$1 list=$2 n=0 batch
  [ -s "$list" ] || { log "no files selected"; return; }
  while true; do
    batch=$(sed -n "$((n*200+1)),$((n*200+200))p" "$list")
    [ -n "$batch" ] || break
    printf '%s\n' "$batch" | sed 's#^#/#' | git -C "$dir" sparse-checkout add --stdin >&2 || \
      printf '%s\n' "$batch" | sed 's#^#/#' | git -C "$dir" sparse-checkout set --no-cone --stdin >&2
    nap; git -C "$dir" checkout >&2
    n=$((n+1))
  done
}

# --- Chronicling Germany (CC BY-NC 4.0) -----------------------------------
# Layout: data/annotations/<Title>_<YYYY-MM-DD>_<NNNN>.xml, data/images/<stem>.jpg
# (Git LFS), data/split.csv (Filename, KoeZ, Test, Validation, OutOfDistribution,
# Advertisements, Pictures). work_in_progress/ and models/ are never used. Held-out
# pages inside Y0..Y1 are selected (all splits; stratum ood, test, validation or train), and
# meta.csv (page_id,title,year,stratum) is written for score.py.
cg_select() {  # split.csv outcsv -> stdout: page ids; writes outcsv (meta.csv)
  Y0=$Y0 Y1=$Y1 python3 - "$1" "$2" <<'PY'
import csv, os, re, sys
src, out = sys.argv[1], sys.argv[2]
y0, y1 = int(os.environ["Y0"]), int(os.environ["Y1"])
yes = {"1", "true", "yes", "x", "t", "y"}
with open(src, newline="", encoding="utf-8") as f:
    dia = csv.Sniffer().sniff(f.read(4096), delimiters=",;\t"); f.seek(0)
    rows = list(csv.DictReader(f, dialect=dia))
res = []
for r in rows:
    on = lambda k: (r.get(k) or "").strip().lower() in yes
    stratum = "ood" if on("OutOfDistribution") else "test" if on("Test") else \
              "validation" if on("Validation") else "train"
    pid = re.sub(r"\.[A-Za-z0-9]+$", "", os.path.basename((r.get("Filename") or "").strip()))
    m = re.search(r"_((?:18|19)\d\d)-\d\d-\d\d(?:_|$)", pid)
    if not (stratum and pid and m and y0 <= int(m.group(1)) <= y1):
        continue
    res.append((pid, pid[:m.start()], m.group(1), stratum))
with open(out, "w", newline="", encoding="utf-8") as f:
    w = csv.writer(f); w.writerow(["page_id", "title", "year", "stratum"])
    w.writerows(sorted(set(res)))
for pid, *_ in sorted(set(res)):
    print(pid)
PY
}
fetch_chronicling() {
  local d="$DATA_ROOT/chronicling" c="$DATA_ROOT/chronicling/.clone"
  mkdir -p "$d"
  sparse_clone "$CG_REPO" "$c"
  git -C "$c" sparse-checkout add /data/split.csv >&2 || true
  git -C "$c" checkout >&2 || true
  if [ ! -f "$c/data/split.csv" ]; then log "no data/split.csv in the clone"; return 1; fi
  local ids; ids=$(mktemp)
  cg_select "$c/data/split.csv" "$d/meta.csv" > "$ids"
  if [ "$LIST" = 1 ]; then
    log "top-level tree:"; git -C "$c" ls-tree --name-only HEAD >&2 || true
    log "split.csv (head):"; { head -5 "$c/data/split.csv" >&2; } || true
    log "held-out pages in $Y0-$Y1: $(wc -l < "$ids"); meta.csv written"
    rm -f "$ids"; return
  fi
  log "chronicling: $(wc -l < "$ids") pages selected"
  local sel; sel=$(mktemp)
  sed 's#^#data/annotations/#; s#$#.xml#' "$ids" > "$sel"
  sed 's#^#data/images/#; s#$#.jpg#' "$ids" >> "$sel"
  checkout_selected "$c" "$sel"; rm -f "$sel"
  keep_licenses "$d" "$c"
  local id n=0 failed=0
  while read -r id; do
    mkdir -p "$d/data/annotations" "$d/data/images"
    [ -e "$d/data/annotations/$id.xml" ] || cp "$c/data/annotations/$id.xml" "$d/data/annotations/$id.xml" || true
    if [ -s "$d/data/images/$id.jpg" ]; then continue; fi
    nap
    if git -C "$c" lfs pull --include="data/images/$id.jpg" >&2 \
       && [ -s "$c/data/images/$id.jpg" ] \
       && ! head -c 60 "$c/data/images/$id.jpg" | grep -q 'git-lfs.github.com/spec'; then
      cp "$c/data/images/$id.jpg" "$d/data/images/$id.jpg"; n=$((n+1))
    else
      log "WARNING: LFS image failed: $id"; failed=$((failed+1))
    fi
  done < "$ids"
  rm -f "$ids"
  log "chronicling: $n images pulled, $failed failed"
  manifest "$d"
  [ "$failed" = 0 ] || log "WARNING: rerun to retry the $failed failed images"
}

# --- Reichsanzeiger-GT (CC0) ----------------------------------------------
# Layout (upstream data/): reichsanzeiger-1820-1939/GT-PAGE/<YYYY>_<issue>_<page>.xml
# (the duplicate *_with-TableRegion tree is ignored), download_images.sh (URL
# base, base64) and imageurls.list ("<film path>/<NNNN>.jp2&CVT=jpeg <YYYY_issue_page>.jpg").
# The image of an XML stem S is the list line whose second field is S.jpg,
# fetched from base + first field into images/<second field>.
ra_urlbase() {  # download_images.sh -> URL base (decoded from its base64 literal)
  sed -n '/urlbase=/,/base64 -d/p' "$1" | tr -d '\n' \
    | sed -n 's/.*echo "\([^"]*\)".*/\1/p' | base64 -d | tr -d '\n'
}
fetch_reichsanzeiger() {
  local d="$DATA_ROOT/reichsanzeiger" c="$DATA_ROOT/reichsanzeiger/.clone"
  mkdir -p "$d"
  sparse_clone "$RA_REPO" "$c"
  git -C "$c" sparse-checkout add /data >&2 || true
  if [ "$LIST" = 1 ]; then
    git -C "$c" checkout >&2 || true
    log "data/ sample:"; { repo_files "$c" | grep '^data/' | head -15 >&2; } || true
    log "data/download_images.sh (head):"; { head -20 "$c/data/download_images.sh" >&2; } || true
    log "data/imageurls.list (head):"; { head -5 "$c/data/imageurls.list" >&2; } || true
    log "PAGE-XML in range: $(repo_files "$c" | grep '/GT-PAGE/.*\.xml$' | grep -v 'with-TableRegion' | while read -r f; do if year_ok "$f"; then echo x; fi; done | wc -l)"
    return
  fi
  nap; git -C "$c" checkout >&2
  keep_licenses "$d" "$c"
  local xmls; xmls=$(mktemp)
  find "$c/data" -name '*.xml' -path '*/GT-PAGE/*' -not -path '*with-TableRegion*' | sort | while read -r f; do
    if year_ok "$f"; then echo "$f"; fi
  done > "$xmls"
  log "reichsanzeiger: $(wc -l < "$xmls") PAGE-XML in $Y0-$Y1"
  while read -r f; do
    mkdir -p "$d/$(dirname "${f#"$c"/}")"
    [ -e "$d/${f#"$c"/}" ] || cp "$f" "$d/${f#"$c"/}"
  done < "$xmls"
  # images: for each selected XML stem, the imageurls.list line naming <stem>.jpg
  local base list failed=0 n=0 suffix name stem
  base=$(ra_urlbase "$c/data/download_images.sh")
  list="$c/data/imageurls.list"
  if [ -z "$base" ] || [ ! -f "$list" ]; then
    log "WARNING: no URL base or imageurls.list under $c/data; no images fetched"
  else
    local stems; stems=$(mktemp)
    sed 's#.*/##; s#\.xml$##' "$xmls" | sort -u > "$stems"
    while read -r suffix name; do
      [ -n "$name" ] || continue
      stem=${name%.*}
      if grep -qxF "$stem" "$stems"; then
        n=$((n+1))
        if ! curl_get "${base}${suffix}" "$d/images/$name"; then
          log "WARNING: image download failed: $name"; failed=$((failed+1))
        fi
      fi
    done < "$list"
    log "reichsanzeiger: $n images selected, $failed failed"
    rm -f "$stems"
  fi
  rm -f "$xmls"
  manifest "$d"
  [ "$failed" = 0 ] || log "WARNING: rerun to retry the $failed failed images"
}

# --- NZZ (zenodo 3333627, CC BY-NC) -----------------------------------------
# One zip, fetched from the direct file URL (never /api: robots.txt disallows
# it) after the 10 s Crawl-delay. Truth is xml/NZZ_groundtruth/nzz_<YYYYMMDD>_..._1.xml;
# the ABBYY_* directories are uncorrected OCR and are not extracted. Images are
# img/*.tif named <NNNN>_nzz_<YYYYMMDD>... or nzz_<YYYYMMDD>..., matched to the
# selected XML on the nzz_<YYYYMMDD> part.
fetch_nzz() {
  local d="$DATA_ROOT/nzz" z="$DATA_ROOT/nzz/.download"
  local zip="$z/NZZ-black-letter-ground-truth-master.zip"
  mkdir -p "$d" "$z"
  if [ "$LIST" = 1 ]; then
    log "nzz: $NZZ_ZIP (after a ${NZZ_DELAY} s crawl delay)"
    [ -s "$zip" ] && { unzip -Z1 "$zip" | head -20 >&2; } || true
    return
  fi
  if [ ! -s "$zip" ]; then log "nzz: Crawl-delay, waiting ${NZZ_DELAY} s"; sleep "$NZZ_DELAY"; fi
  curl_get "$NZZ_ZIP" "$zip"
  local list="$z/members.list" xs="$z/xml.sel" keys="$z/keys.txt" sel="$z/members.sel"
  unzip -Z1 "$zip" > "$list"
  grep -E '(^|/)NZZ_groundtruth/[^/]+\.xml$' "$list" | while read -r m; do
    if year_ok "$m"; then echo "$m"; fi
  done > "$xs" || true
  grep -oE 'nzz_[0-9]{8}' "$xs" | sort -u > "$keys" || true
  { cat "$xs"
    grep -iE '(^|/)img/[^/]+\.tiff?$' "$list" | grep -F -f "$keys" || true
    grep -iE '(^|/)(LICEN[CS]E|README)[^/]*$' "$list" || true; } > "$sel"
  log "nzz: $(wc -l < "$xs") truth XML, $(grep -ciE '\.tiff?$' "$sel" || true) images selected"
  python3 - "$zip" "$sel" "$d" <<'PY'
import sys, zipfile
zf, sel, out = sys.argv[1:4]
with zipfile.ZipFile(zf) as z, open(sel, encoding="utf-8") as f:
    for name in (l.rstrip("\n") for l in f if l.strip()):
        z.extract(name, out)
PY
  { echo "Zenodo record $NZZ_RECORD; CC BY-NC 4.0; evaluation only, not redistributed."; } > "$d/LICENSE.txt"
  manifest "$d"
}

# --- Austrian Newspapers (fixed XML on GitHub + images on zenodo) -----------
# zenodo /api is disallowed by robots.txt, so give the record's file URLs from
# https://zenodo.org/records/$AT_RECORD in AT_URLS (space separated).
fetch_austrian() {
  local d="$DATA_ROOT/austrian" c="$DATA_ROOT/austrian/.clone" z="$DATA_ROOT/austrian/.download"
  mkdir -p "$d" "$z"
  sparse_clone "$AT_REPO" "$c"
  if [ "$LIST" = 1 ]; then
    log "repo files:"; { repo_files "$c" | head -30 >&2; } || true
    log "zenodo record: https://zenodo.org/records/$AT_RECORD (give file URLs in AT_URLS)"; return
  fi
  local sel; sel=$(mktemp)
  # ADJUST: the "fixed" XML directory name once the layout is seen
  repo_files "$c" | grep -iE '\.xml$' | while read -r f; do if year_ok "$f"; then echo "$f"; fi; done > "$sel"
  checkout_selected "$c" "$sel"; rm -f "$sel"
  keep_licenses "$d" "$c"
  rsync -a --exclude .git --exclude .clone --exclude .download "$c"/ "$d"/ 2>/dev/null || cp -rn "$c"/. "$d"/
  [ -n "$AT_URLS" ] || log "austrian: AT_URLS empty, no images fetched"
  local url name
  for url in $AT_URLS; do
    name=${url%%\?*}; name=${name##*/}
    case "$name" in
      *.zip) curl_get "$url" "$z/$name"
             unzip -Z1 "$z/$name" | while read -r m; do if year_ok "$m"; then echo "$m"; fi; done > "$z/$name.sel"
             if [ -s "$z/$name.sel" ]; then
               python3 - "$z/$name" "$z/$name.sel" "$d/images" <<'PY'
import sys, zipfile
zf, sel, out = sys.argv[1:4]
with zipfile.ZipFile(zf) as z, open(sel, encoding="utf-8") as f:
    for name in (l.rstrip("\n") for l in f if l.strip()):
        z.extract(name, out)
PY
             fi ;;
      *.jpg|*.jpeg|*.png|*.tif|*.tiff) if year_ok "$name"; then curl_get "$url" "$d/images/$name"; fi ;;
      LICENSE*|README*) curl_get "$url" "$d/$name" ;;
    esac
  done
  manifest "$d"
}

# --- main ---------------------------------------------------------------------
sets=()
while [ $# -gt 0 ]; do
  case "$1" in
    --list) LIST=1 ;;
    --with-austrian) AUSTRIAN=1 ;;
    --i-am-a-person) PERSON=1 ;;
    -h|--help) sed -n '2,/^set -/p' "$0" | sed '$d'; exit 0 ;;
    --set)
      shift
      case "${1:-}" in
        all) sets+=(chronicling reichsanzeiger nzz) ;;
        chronicling|reichsanzeiger|nzz|austrian) sets+=("$1") ;;
        *) echo "unknown set: ${1:-}" >&2; exit 2 ;;
      esac ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done
[ "$AUSTRIAN" = 1 ] && sets+=(austrian)
[ ${#sets[@]} -gt 0 ] || { echo "usage: DATA_ROOT=dir $0 [--list] [--i-am-a-person] [--with-austrian] --set {chronicling|reichsanzeiger|nzz|austrian|all} [--set ...]" >&2; exit 2; }
[ "$(awk -v s="$SLEEP" 'BEGIN{print (s>=1.5)}')" = 1 ] || { echo "SLEEP must be >= 1.5" >&2; exit 2; }
[ "$(awk -v s="$NZZ_DELAY" 'BEGIN{print (s>=10)}')" = 1 ] || { echo "NZZ_DELAY must be >= 10" >&2; exit 2; }
if [ "$PERSON" != 1 ] && [ "$LIST" != 1 ]; then
  for s in "${sets[@]}"; do
    case "$s" in
      chronicling|reichsanzeiger)
        echo "refusing: $s downloads from hosts whose robots.txt disallows it" \
             "(gitlab.uni-bonn.de /*/*.git/*, digi.bib.uni-mannheim.de /reichsanzeiger.fcgi)." \
             "A person must run it: pass --i-am-a-person." >&2
        exit 2 ;;
    esac
  done
fi
for s in "${sets[@]}"; do log "== $s =="; "fetch_$s"; done
