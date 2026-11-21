#!/usr/bin/env bash
# Polite, resumable subset downloaders for the Part A German Fraktur truth sets.
#
#   DATA_ROOT=/data ./fetch.sh --list chronicling      # show layout, download nothing big
#   DATA_ROOT=/data ./fetch.sh chronicling reichsanzeiger nzz
#   DATA_ROOT=/data ./fetch.sh --with-austrian all
#
# Each set lands in $DATA_ROOT/<set>/ with its LICENSE file(s) and MANIFEST.tsv
# (sha256, bytes, path). >= 1 s sleep between requests. Rerunning resumes
# (git sparse clone is updated in place, curl uses -C -, existing files skipped).
# Nothing is redistributed: CC BY-NC sets (chronicling, nzz, austrian) are for
# evaluation only.
#
# The year filter is the function year_ok below. It looks for a 4-digit
# 18xx/19xx year in the path, or in YEAR_MAP (a TSV "page_id<TAB>year", set
# $CG_YEAR_MAP) when the layout carries years elsewhere. Run --list first and
# adjust year_ok / the *_select functions to the layout actually seen.
set -euo pipefail

: "${DATA_ROOT:?set DATA_ROOT}"
Y0=${Y0:-1850}; Y1=${Y1:-1920}
SLEEP=${SLEEP:-1.5}                 # seconds between requests (>= 1)
CG_REPO=${CG_REPO:-https://gitlab.uni-bonn.de/digital-history/Chronicling-Germany-Dataset.git}
RA_REPO=${RA_REPO:-https://github.com/UB-Mannheim/reichsanzeiger-gt.git}
AT_REPO=${AT_REPO:-https://github.com/UB-Mannheim/AustrianNewspapers.git}
NZZ_RECORD=${NZZ_RECORD:-3333627}
AT_RECORD=${AT_RECORD:-3387369}
CG_YEAR_MAP=${CG_YEAR_MAP:-}
UA="german-truth-fetch/1 (research; polite)"
LIST=0; AUSTRIAN=0

log() { printf '[fetch] %s\n' "$*" >&2; }
nap() { sleep "$SLEEP"; }

# --- year filter: reads paths on stdin, prints those inside Y0..Y1 --------
year_of() {  # path -> year or empty
  local p=$1 y=""
  if [ -n "$CG_YEAR_MAP" ] && [ -f "$CG_YEAR_MAP" ]; then
    local id; id=$(basename "${p%.*}")
    y=$(awk -F'\t' -v id="$id" '$1==id{print $2; exit}' "$CG_YEAR_MAP")
  fi
  [ -n "$y" ] || y=$(printf '%s' "$p" | grep -oE '(^|[^0-9])(18[0-9]{2}|19[0-9]{2})([^0-9]|$)' | head -1 | grep -oE '[0-9]{4}' || true)
  printf '%s' "$y"
}
year_ok() {
  local y; y=$(year_of "$1")
  [ -n "$y" ] && [ "$y" -ge "$Y0" ] && [ "$y" -le "$Y1" ]
}

curl_get() {  # url out : resumable, polite, retry
  local url=$1 out=$2
  mkdir -p "$(dirname "$out")"
  nap
  curl -fL --retry 5 --retry-delay 5 -C - -A "$UA" -o "$out" "$url"
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
cg_select() {  # clonedir -> stdout: repo paths to fetch (images + PAGE-XML in year range)
  local dir=$1
  # ADJUST once --list shows the layout: which extensions/directories are
  # pages, and how OOD test pages are marked (score.py looks for an "ood"
  # path component or --meta).
  repo_files "$dir" | grep -iE '\.(xml|png|jpe?g|tiff?)$' | while read -r f; do
    year_ok "$f" && echo "$f"
  done
}
fetch_chronicling() {
  local d="$DATA_ROOT/chronicling" c="$DATA_ROOT/chronicling/.clone"
  mkdir -p "$d"
  sparse_clone "$CG_REPO" "$c"
  if [ "$LIST" = 1 ]; then
    log "top-level tree:"; git -C "$c" ls-tree --name-only HEAD >&2
    log "directories (depth 2):"; repo_files "$c" | awk -F/ 'NF>1{print $1"/"$2}' | sort | uniq -c | head -60 >&2
    log "sample files:"; repo_files "$c" | head -20 >&2
    log "README (head):"; head -80 "$c"/README* >&2 2>/dev/null || true
    log "selected by year_ok: $(cg_select "$c" | wc -l) files"
    return
  fi
  local sel; sel=$(mktemp); cg_select "$c" > "$sel"
  log "chronicling: $(wc -l < "$sel") files selected"
  checkout_selected "$c" "$sel"; rm -f "$sel"
  keep_licenses "$d" "$c"
  # expose checked-out files at the set root without the .git dir getting scanned
  rsync -a --exclude .git --exclude .clone "$c"/ "$d"/ 2>/dev/null || cp -rn "$c"/. "$d"/ 
  manifest "$d"
}

# --- Reichsanzeiger-GT (CC0) ----------------------------------------------
fetch_reichsanzeiger() {
  local d="$DATA_ROOT/reichsanzeiger" c="$DATA_ROOT/reichsanzeiger/.clone"
  mkdir -p "$d"
  sparse_clone "$RA_REPO" "$c"
  git -C "$c" sparse-checkout add /download_images.sh /data >&2 || true
  if [ "$LIST" = 1 ]; then
    git -C "$c" checkout >&2 || true
    log "data/ sample:"; repo_files "$c" | grep '^data/' | head -15 >&2
    log "download_images.sh (head):"; head -20 "$c/download_images.sh" >&2 || true
    log "PAGE-XML in range: $(repo_files "$c" | grep '^data/.*\.xml$' | while read -r f; do year_ok "$f" && echo x; done | wc -l)"
    return
  fi
  nap; git -C "$c" checkout >&2
  keep_licenses "$d" "$c"
  local xmls; xmls=$(mktemp)
  find "$c/data" -name '*.xml' | while read -r f; do year_ok "$f" && echo "$f"; done > "$xmls"
  log "reichsanzeiger: $(wc -l < "$xmls") PAGE-XML in $Y0-$Y1"
  while read -r f; do mkdir -p "$d/$(dirname "${f#$c/}")"; cp -n "$f" "$d/${f#$c/}"; done < "$xmls"
  # images: URLs listed in download_images.sh; keep those whose URL (or the
  # stem of a selected XML) falls in range. ADJUST if the script's format differs.
  local urls; urls=$(mktemp)
  grep -oE "https?://[^ \"']+\.(jpe?g|png|tiff?|jp2)" "$c/download_images.sh" | sort -u > "$urls" || true
  local stems; stems=$(mktemp)
  sed 's#.*/##; s#\.xml$##' "$xmls" | sort -u > "$stems"
  while read -r u; do
    stem=$(basename "${u%.*}")
    if grep -qxF "$stem" "$stems" || year_ok "$u"; then
      out="$d/images/$(basename "$u")"
      [ -s "$out" ] && [ ! -f "$out.part" ] && continue
      curl_get "$u" "$out"
    fi
  done < "$urls"
  rm -f "$xmls" "$urls" "$stems"
  manifest "$d"
}

# --- zenodo helper: list a record's files via the API -----------------------
zenodo_files() {  # record -> "name<TAB>url" lines
  nap
  curl -fsSL -A "$UA" "https://zenodo.org/api/records/$1" | python3 -c '
import sys, json
d = json.load(sys.stdin)
print("#license\t" + str(d.get("metadata", {}).get("license")))
for f in d.get("files", []):
    print(f["key"] + "\t" + f["links"]["self"] + "\t" + str(f.get("size")))'
}

# --- NZZ (zenodo 3333627, CC BY-NC) -----------------------------------------
fetch_nzz() {
  local d="$DATA_ROOT/nzz" z="$DATA_ROOT/nzz/.download"
  mkdir -p "$d" "$z"
  local files; files=$(zenodo_files "$NZZ_RECORD")
  if [ "$LIST" = 1 ]; then printf '%s\n' "$files" >&2; return; fi
  printf '%s\n' "$files" | grep -v '^#' | grep -i '\.zip' | while IFS=$'\t' read -r name url size; do
    curl_get "$url" "$z/$name"
    unzip -Z1 "$z/$name" > "$z/$name.list"
    # ADJUST if members do not carry the year in their path (use CG_YEAR_MAP-style map)
    while read -r m; do year_ok "$m" && echo "$m"; done < "$z/$name.list" > "$z/$name.sel"
    # also keep LICENSE/README members
    grep -iE '(^|/)(LICEN[CS]E|README)' "$z/$name.list" >> "$z/$name.sel" || true
    log "nzz: $(wc -l < "$z/$name.sel") members selected from $name"
    [ -s "$z/$name.sel" ] && unzip -qn "$z/$name" -d "$d" $(sed 's/[][*?\\]/\\&/g; s/ /\\ /g' "$z/$name.sel" | tr '\n' ' ') || true
  done
  printf '%s\n' "$files" | grep -m1 '^#license' > "$d/LICENSE.txt" || true
  echo "Zenodo record $NZZ_RECORD; CC BY-NC; evaluation only, not redistributed." >> "$d/LICENSE.txt"
  manifest "$d"
}

# --- Austrian Newspapers (fixed XML on GitHub + images on zenodo) -----------
fetch_austrian() {
  local d="$DATA_ROOT/austrian" c="$DATA_ROOT/austrian/.clone" z="$DATA_ROOT/austrian/.download"
  mkdir -p "$d" "$z"
  sparse_clone "$AT_REPO" "$c"
  local files; files=$(zenodo_files "$AT_RECORD")
  if [ "$LIST" = 1 ]; then
    log "repo files:"; repo_files "$c" | head -30 >&2
    log "zenodo files:"; printf '%s\n' "$files" >&2; return
  fi
  local sel; sel=$(mktemp)
  # ADJUST: the "fixed" XML directory name once the layout is seen
  repo_files "$c" | grep -iE '\.xml$' | while read -r f; do year_ok "$f" && echo "$f"; done > "$sel"
  checkout_selected "$c" "$sel"; rm -f "$sel"
  keep_licenses "$d" "$c"
  rsync -a --exclude .git --exclude .clone --exclude .download "$c"/ "$d"/ 2>/dev/null || cp -rn "$c"/. "$d"/
  printf '%s\n' "$files" | grep -v '^#' | while IFS=$'\t' read -r name url size; do
    case "$name" in
      *.zip) curl_get "$url" "$z/$name"
             unzip -Z1 "$z/$name" | while read -r m; do year_ok "$m" && echo "$m"; done > "$z/$name.sel"
             [ -s "$z/$name.sel" ] && unzip -qn "$z/$name" -d "$d/images" $(sed 's/ /\\ /g' "$z/$name.sel" | tr '\n' ' ') || true ;;
      *.jpg|*.jpeg|*.png|*.tif|*.tiff) year_ok "$name" && curl_get "$url" "$d/images/$name" || true ;;
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
    -h|--help) sed -n '2,15p' "$0"; exit 0 ;;
    all) sets+=(chronicling reichsanzeiger nzz) ;;
    chronicling|reichsanzeiger|nzz|austrian) sets+=("$1") ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done
[ "$AUSTRIAN" = 1 ] && sets+=(austrian)
[ ${#sets[@]} -gt 0 ] || { echo "usage: DATA_ROOT=dir $0 [--list] [--with-austrian] {chronicling|reichsanzeiger|nzz|austrian|all}..." >&2; exit 2; }
[ "$(printf '%s' "$SLEEP" | awk '{print ($1>=1)}')" = 1 ] || { echo "SLEEP must be >= 1" >&2; exit 2; }
for s in "${sets[@]}"; do log "== $s =="; "fetch_$s"; done
