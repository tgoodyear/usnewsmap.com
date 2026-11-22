#!/usr/bin/env python3
"""Score Tesseract on German Fraktur page ground truth (Part A of the German truth protocol).

Stdlib + Pillow only. One CSV row per (page, arm); never any OCR or GT text.

Per page: reference text from PAGE-XML (TextLine/TextEquiv/Unicode, regions in
reading order) or ALTO (TextBlock > TextLine > String@CONTENT); hypothesis from
a configurable tesseract command; CER raw and folded; word F1; child CPU
seconds (RUSAGE_CHILDREN delta around the tesseract call).

Text rules (see README.md):
  raw    : NFC, CR dropped, all whitespace runs collapsed to one space.
  folded : NFC; long s -> s; U+0292 (tailed z) -> z; U+0364 -> U+0308;
           a hyphen mark ('-', U+00AC, U+2E17) after a word character and
           before newline + word character is dehyphenated (mark and line
           break vanish, left to right, no overlap); any remaining U+00AC /
           U+2E17 -> '-'; then whitespace collapse.
  cer_*  = edits / ref_chars, where ref_chars is the length of the RAW
           normalised reference (one shared denominator for both CERs).
  word_f1: bag of words (letter runs with inner apostrophes, case kept) of the
           FOLDED texts; F1 = 2*overlap / (n_ref + n_hyp).
"""
import argparse, csv, hashlib, json, multiprocessing as mp, os, re, resource
import shlex, subprocess, sys, tempfile, time, unicodedata
import xml.etree.ElementTree as ET
from collections import Counter

FIELDS = ["set", "stratum", "page_id", "title", "year", "arm", "ref_chars",
          "edits_raw", "edits_folded", "cer_raw", "cer_folded", "word_f1",
          "cpu_s", "gpu_study_flag"]
DEFAULT_CMD = ("tesseract {img} stdout -l {model} --psm 3 --dpi 300 "
               "--tessdata-dir {tessdata}")
PODMAN_EXAMPLE = (
    "podman run --rm --network=none --user 10001:10001 -v {imgdir}:/in:ro,Z "
    "IMAGE@sha256:DIGEST /in/{imgname} stdout -l {model} --psm 3 --dpi 300")
EXPECTED_SHA = {"frak2021":
    "1da2384254fa8462c776faf3b43307fe19ce51be0931c623b2fdd560f96e299a"}
IMG_EXT = (".png", ".jpg", ".jpeg", ".tif", ".tiff", ".jp2", ".bmp")
PLAIN_EXT = (".png", ".jpg", ".jpeg", ".tif", ".tiff", ".bmp")
# a year, but not an end of a range such as the directory name 1820-1939
# (also the first 4 digits of an 8-digit date such as nzz_18910302)
YEAR_RE = re.compile(r"(?<!\d)(?<!\d{4}-)(1[89]\d\d)"
                     r"(?:(?!\d)|(?=(?:0[1-9]|1[0-2])(?:0[1-9]|[12]\d|3[01])(?!\d)))(?!-\d{4})")
SKIP_DIRS = ("work_in_progress", "models")     # Chronicling Germany, not pages
NZZ_KEY_RE = re.compile(r"nzz_\d{8}")
PRUNE_DIRS = (".clone", ".git", ".download")   # downloader / VCS dirs, never pages
NZZ_TRUTH_DIR = "NZZ_groundtruth"   # the archive also holds uncorrected ABBYY XML

# ---------------------------------------------------------------- text ----
def _is_word(c):
    return c == "_" or c.isalnum()

def collapse(s):
    return " ".join(s.split())

def norm_raw(s):
    return collapse(unicodedata.normalize("NFC", s.replace("\r", "")))

_HYPH = ("-", "¬", "⸗")

def norm_fold(s):
    s = unicodedata.normalize("NFC", s.replace("\r", ""))
    s = s.replace("\u017f", "s").replace("\u0292", "z").replace("\u0364", "\u0308")
    s = unicodedata.normalize("NFC", s)   # a + U+0308 -> precomposed, like OCR output
    cs, out, i = list(s), [], 0
    while i < len(cs):
        if _is_word(cs[i]) and i + 1 < len(cs) and cs[i + 1] in _HYPH:
            j, nl = i + 2, False
            while j < len(cs) and cs[j].isspace():
                nl = nl or cs[j] == "\n"
                j += 1
            if nl and j < len(cs) and _is_word(cs[j]):
                out += [cs[i], cs[j]]
                i = j + 1
                continue
        out.append(cs[i])
        i += 1
    return collapse("".join(out).replace("¬", "-").replace("⸗", "-"))

def lev(a, b):
    """Levenshtein distance (Hyyro bit-parallel, Python big ints)."""
    if a == b:
        return 0
    if not a:
        return len(b)
    if not b:
        return len(a)
    m = len(a)
    peq = {}
    for i, c in enumerate(a):
        peq[c] = peq.get(c, 0) | (1 << i)
    mask, high = (1 << m) - 1, 1 << (m - 1)
    vp, vn, score = mask, 0, m
    for c in b:
        eq = peq.get(c, 0)
        xv = eq | vn
        xh = ((((eq & vp) + vp) ^ vp) | eq) & mask
        hp = (vn | ~(xh | vp)) & mask
        hn = vp & xh
        if hp & high:
            score += 1
        elif hn & high:
            score -= 1
        hp = ((hp << 1) | 1) & mask
        hn = (hn << 1) & mask
        vp = (hn | ~(xv | hp)) & mask
        vn = hp & xv
    return score

WORD_RE = re.compile(r"[^\W\d_]+(?:['’][^\W\d_]+)*")

def word_f1(ref, hyp):
    r, h = Counter(WORD_RE.findall(ref)), Counter(WORD_RE.findall(hyp))
    nr, nh = sum(r.values()), sum(h.values())
    if nr + nh == 0:
        return 1.0
    return 2.0 * sum((r & h).values()) / (nr + nh)

def score_texts(ref, hyp):
    rr, hr = norm_raw(ref), norm_raw(hyp)
    rf, hf = norm_fold(ref), norm_fold(hyp)
    n = len(rr)
    er, ef = lev(rr, hr), lev(rf, hf)
    return {"ref_chars": n, "edits_raw": er, "edits_folded": ef,
            "cer_raw": er / n if n else 0.0, "cer_folded": ef / n if n else 0.0,
            "word_f1": word_f1(rf, hf)}

# ----------------------------------------------------------- GT readers ----
def _ln(el):
    return el.tag.rsplit("}", 1)[-1]

def _kids(el, name):
    return [c for c in el if _ln(c) == name]

def _line_text_page(line):
    for te in _kids(line, "TextEquiv"):
        for u in _kids(te, "Unicode"):
            return u.text or ""
    return None

_GROUPS = ("OrderedGroup", "OrderedGroupIndexed", "UnorderedGroup", "UnorderedGroupIndexed")
_REFS = ("RegionRefIndexed", "RegionRef")

def _index(el):
    try:
        return int(el.get("index") or 0)
    except ValueError:
        return 0

def _walk_order(el, out):
    """Region ids in reading order. Indexes are local to their group, so only
    the children of one ordered group are sorted together (stable); unordered
    groups keep document order."""
    kids = [c for c in el if _ln(c) in _GROUPS or _ln(c) in _REFS]
    if _ln(el).startswith("OrderedGroup"):
        kids.sort(key=_index)
    for c in kids:
        if _ln(c) in _REFS:
            out.append(c.get("regionRef"))
        else:
            _walk_order(c, out)

def ref_from_page(root):
    """Lines (Unicode of each TextLine) joined by \\n, regions in reading order."""
    regions = {}
    for el in root.iter():
        if _ln(el) == "TextRegion" and el.get("id"):
            regions[el.get("id")] = el
    order = []
    for ro in root.iter():
        if _ln(ro) == "ReadingOrder":
            _walk_order(ro, order)
            break
    top = [el for el in root.iter() if _ln(el) == "TextRegion"]
    seq, seen = [], set()
    for rid in order:
        if rid in regions and rid not in seen:
            seq.append(regions[rid]); seen.add(rid)
    for el in top:                      # regions the order missed, document order
        if el.get("id") not in seen:
            seq.append(el); seen.add(el.get("id"))
    lines, done = [], set()
    for reg in seq:
        for ln in reg.iter():
            if _ln(ln) == "TextLine" and id(ln) not in done:
                done.add(id(ln))
                t = _line_text_page(ln)
                if t is not None:
                    lines.append(t)
    return "\n".join(lines)

def ref_from_alto(root):
    lines = []
    for ln in root.iter():
        if _ln(ln) != "TextLine":
            continue
        # words are space separated; a HYP attaches to the preceding word
        txt = ""
        for ch in ln:
            n = _ln(ch)
            if n == "String":
                txt += (" " if txt else "") + (ch.get("CONTENT") or "")
            elif n == "HYP":
                txt += ch.get("CONTENT") or "-"
        lines.append(txt)
    return "\n".join(lines)

def read_reference(xml_path):
    root = ET.parse(xml_path).getroot()
    kind = _ln(root)
    if kind == "alto":
        return ref_from_alto(root)
    return ref_from_page(root)

# --------------------------------------------------------- page discovery ----
def year_of(*texts):
    for t in texts:
        m = YEAR_RE.search(t or "")
        if m:
            return int(m.group(1))
    return None

def title_of(set_name, page_id):
    """Title for title-clustered bootstrap. Adjust per layout, or use --meta."""
    if set_name == "reichsanzeiger":
        return "Deutscher Reichsanzeiger"
    if set_name == "nzz":
        return "Neue Zuercher Zeitung"
    m = YEAR_RE.search(page_id)
    head = page_id[:m.start()] if m else re.split(r"[_]", page_id)[0]
    head = head.strip("_- ")
    return head or set_name

def stratum_of(set_name, relpath):
    """ood for the Chronicling Germany out-of-distribution test pages."""
    if set_name.startswith("chronicling") and re.search(r"(^|[/_\-.])ood([/_\-.]|$)",
                                                        relpath.lower()):
        return "ood"
    return "test"

def load_meta(path):
    """Optional TSV/CSV: page_id, title, year, stratum (header row required)."""
    meta = {}
    if not path:
        return meta
    with open(path, newline="", encoding="utf-8") as f:
        sample = f.read(2048); f.seek(0)
        dia = csv.Sniffer().sniff(sample, delimiters=",\t")
        for r in csv.DictReader(f, dialect=dia):
            meta[r["page_id"]] = r
    return meta

def find_pages(set_name, root_dir, meta, years, keep_unknown):
    imgs = {}
    xmls = []
    for dp, dn, fn in os.walk(root_dir):
        skip = PRUNE_DIRS + (SKIP_DIRS if set_name.startswith("chronicling") else ())
        dn[:] = sorted(d for d in dn if d not in skip)
        for f in sorted(fn):
            p = os.path.join(dp, f)
            low = f.lower()
            if low.endswith(IMG_EXT):
                stem = os.path.splitext(f)[0]
                imgs.setdefault(stem, p)
                if set_name == "nzz":   # <NNNN>_nzz_<date>... or nzz_<date>...
                    imgs.setdefault(re.sub(r"^\d+_(?=nzz_)", "", stem), p)
                    m = NZZ_KEY_RE.search(stem)
                    if m:
                        imgs.setdefault("key:" + m.group(0), p)
            elif low.endswith(".xml") and "license" not in low:
                if set_name == "nzz" and NZZ_TRUTH_DIR not in \
                        os.path.relpath(p, root_dir).split(os.sep)[:-1]:
                    continue            # only the corrected truth counts
                xmls.append(p)
    pages, skipped, missing, seen = [], 0, [], set()
    for x in sorted(xmls):
        pid = os.path.splitext(os.path.basename(x))[0]
        if pid in seen:                 # same page twice (e.g. a copied tree)
            continue
        img = imgs.get(pid) or imgs.get(re.sub(r"\.(gt|xml)$", "", pid))
        if not img and set_name == "nzz":
            m = NZZ_KEY_RE.search(pid)
            img = imgs.get("key:" + m.group(0)) if m else None
        rel = os.path.relpath(x, root_dir)
        m = meta.get(pid, {})
        year = int(m["year"]) if m.get("year") else year_of(pid, rel)
        if year is None and not keep_unknown or (year is not None and
                not years[0] <= year <= years[1]):
            skipped += 1                # year eligibility is decided first
            continue
        seen.add(pid)
        if not img:                     # eligible page without an image
            missing.append(pid)
            continue
        pages.append({"set": set_name, "page_id": pid, "xml": x, "img": img,
                      "year": year if year is not None else "",
                      "title": m.get("title") or title_of(set_name, pid),
                      "stratum": m.get("stratum") or stratum_of(set_name, rel)})
    return pages, skipped, missing

# ------------------------------------------------------------- tesseract ----
def sha256_file(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()

def build_cmd(template, img, model, tessdata):
    q = shlex.quote
    return shlex.split(template.format(
        img=q(img), imgdir=q(os.path.dirname(os.path.abspath(img))),
        imgname=q(os.path.basename(img)), model=q(model), tessdata=q(tessdata)))

def cpu_children():
    r = resource.getrusage(resource.RUSAGE_CHILDREN)
    return r.ru_utime + r.ru_stime

def cgroup_cpu(path):
    """usage_usec of a cgroup (dir holding cpu.stat, or the file itself), in s."""
    p = os.path.join(path, "cpu.stat") if os.path.isdir(path) else path
    with open(p, encoding="utf-8") as f:
        for line in f:
            k, _, v = line.partition(" ")
            if k == "usage_usec":
                return int(v) / 1e6
    raise ValueError("no usage_usec in %s" % p)

def run_tesseract(template, img, model, tessdata, timeout, cgroup=None):
    clock = (lambda: cgroup_cpu(cgroup)) if cgroup else cpu_children
    t0 = clock()
    p = subprocess.run(build_cmd(template, img, model, tessdata),
                       stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                       timeout=timeout)
    cpu = clock() - t0
    if p.returncode != 0:
        raise RuntimeError("tesseract exit %d: %s" % (
            p.returncode, p.stderr.decode("utf-8", "replace")[-300:]))
    return p.stdout.decode("utf-8", "replace"), cpu

def prepared_image(img, tmpdir):
    """Pillow: convert formats leptonica may lack (e.g. jp2) to PNG."""
    if img.lower().endswith(PLAIN_EXT):
        return img
    from PIL import Image
    out = os.path.join(tmpdir, os.path.splitext(os.path.basename(img))[0] + ".png")
    with Image.open(img) as im:
        im.save(out)
    return out

GPU_EXT = re.compile(r"\.(xml|gt|jpe?g|png|tiff?|jp2)$", re.I)

def gpu_norm(token):
    """Normalise a GPU-study list entry or page id to `<stem>` or `<set>/<stem>`:
    CR/BOM/whitespace stripped, extensions dropped, NZZ `<NNNN>_nzz_` prefix
    removed."""
    t = token.replace("\r", "").replace("\ufeff", "").strip().replace("\\", "/")
    parts = [p for p in t.split("/") if p]
    if not parts:
        return ""
    stem = parts[-1]
    while GPU_EXT.search(stem):
        stem = GPU_EXT.sub("", stem)
    stem = re.sub(r"^\d+_(?=nzz_)", "", stem)
    return stem if len(parts) == 1 else parts[0] + "/" + stem

def gpu_flag(set_name, page_id, gpu):
    stem = gpu_norm(page_id)
    return int(stem in gpu or "%s/%s" % (set_name, stem) in gpu)

def job(args):
    page, arm, template, tessdata, timeout, gpu, cgroup = args
    try:
        ref = read_reference(page["xml"])
        with tempfile.TemporaryDirectory() as td:
            img = prepared_image(page["img"], td)
            hyp, cpu = run_tesseract(template, img, arm, tessdata, timeout, cgroup)
        s = score_texts(ref, hyp)
    except Exception as e:  # not recorded, so a rerun retries it
        return None, "%s/%s [%s]: %s" % (page["set"], page["page_id"], arm,
                                         str(e).replace("\n", " ")[:200])
    row = {k: page[k] for k in ("set", "stratum", "page_id", "title", "year")}
    row.update(arm=arm, cpu_s=round(cpu, 3), **s)
    for k in ("cer_raw", "cer_folded", "word_f1"):
        row[k] = round(row[k], 6)
    row["gpu_study_flag"] = gpu_flag(page["set"], page["page_id"], gpu)
    return row, None

def done_keys(path):
    if not os.path.exists(path):
        return set()
    with open(path, newline="", encoding="utf-8") as f:
        return {(r["set"], r["page_id"], r["arm"]) for r in csv.DictReader(f)}

def resume_problems(old, new, csv_arms):
    """Why an existing run (its sidecar `old`) cannot be continued with `new`."""
    out = []
    if old.get("limit"):
        out.append("the earlier run used --limit %s (use a new --out for a "
                   "full run)" % old["limit"])
    elif new.get("limit"):
        out.append("--limit now, but the earlier run had none (use a new --out)")
    oarms = old.get("arms")
    if not isinstance(oarms, dict):
        return ["sidecar has no arm provenance"]
    for arm, sha in new["arms"].items():
        if arm in oarms and oarms[arm] != sha:
            out.append("model %s sha256 changed (%s -> %s)" % (arm, oarms[arm], sha))
    for arm in sorted(csv_arms - set(oarms)):
        out.append("CSV has rows for arm %s that the sidecar does not record" % arm)
    for k, label in (("tesseract_version", "tesseract version"), ("cmd", "command"),
                     ("cpu_cgroup", "cpu cgroup"),
                     ("gpu_study_sha256", "GPU-study exclusion list")):
        if old.get(k) != new.get(k):
            out.append("%s changed (%r -> %r)" % (label, old.get(k), new.get(k)))
    return out

def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--data-root", default=os.environ.get("DATA_ROOT"),
                    help="root holding <set>/ directories (default $DATA_ROOT)")
    ap.add_argument("--sets", default="chronicling,reichsanzeiger,nzz",
                    help="comma list of set directory names under data root")
    ap.add_argument("--out", required=True, help="CSV to append to (resumable)")
    ap.add_argument("--arms", default="frak2021,deu_latf",
                    help="tessdata model names, comma list")
    ap.add_argument("--tessdata", default=os.environ.get("TESSDATA_PREFIX", "."))
    ap.add_argument("--cmd", default=DEFAULT_CMD,
                    help="command template; fields {img} {imgdir} {imgname} "
                         "{model} {tessdata}. Podman example: " + PODMAN_EXAMPLE)
    ap.add_argument("--version-cmd", default="tesseract --version")
    ap.add_argument("--workers", type=int, default=1)
    ap.add_argument("--cpu-cgroup", metavar="PATH",
                    help="cgroup (dir with cpu.stat) of the engine, e.g. the podman "
                         "container's: cpu_s = usage_usec delta around each page "
                         "(needs --workers 1; default: client CPU via RUSAGE_CHILDREN)")
    ap.add_argument("--years", default="1850-1920")
    ap.add_argument("--keep-unknown-year", action="store_true")
    ap.add_argument("--meta", help="CSV/TSV with page_id,title,year,stratum")
    ap.add_argument("--gpu-study-pages",
                    help="file of page ids (or set/page_id), one per line")
    ap.add_argument("--allow-missing-images", action="store_true",
                    help="score anyway when eligible pages have no image; they are "
                         "recorded as missing and analyze.py reports INCOMPLETE")
    ap.add_argument("--limit", type=int, default=0, help="max pages per set")
    ap.add_argument("--timeout", type=int, default=900)
    a = ap.parse_args(argv)
    if not a.data_root:
        ap.error("--data-root or $DATA_ROOT required")
    y0, y1 = (int(x) for x in a.years.split("-"))
    gpu = set()
    if a.gpu_study_pages:
        with open(a.gpu_study_pages, encoding="utf-8") as f:
            gpu = {gpu_norm(l) for l in f
                   if l.strip().lstrip("\ufeff") and not l.lstrip().startswith("#")}
            gpu.discard("")
    arms = [x for x in a.arms.split(",") if x]
    if a.cpu_cgroup and a.workers != 1:
        ap.error("--cpu-cgroup needs --workers 1 (a shared cgroup mixes pages)")
    meta_all = load_meta(a.meta)

    gpu_ids = sorted(gpu)
    info = {"arms": {}, "cmd": a.cmd, "years": a.years, "limit": a.limit,
            "cpu_cgroup": a.cpu_cgroup,
            "gpu_study_ids": gpu_ids,
            "gpu_study_sha256": hashlib.sha256(
                "\n".join(gpu_ids).encode("utf-8")).hexdigest(),
            "keep_unknown_year": a.keep_unknown_year}
    for arm in arms:
        p = os.path.join(a.tessdata, arm + ".traineddata")
        sha = sha256_file(p) if os.path.exists(p) else None
        info["arms"][arm] = sha
        if sha and arm in EXPECTED_SHA and sha != EXPECTED_SHA[arm]:
            print("WARNING: %s sha256 %s differs from the deployed model" % (arm, sha),
                  file=sys.stderr)
        if sha is None:
            print("WARNING: %s not found (fine when the template mounts its own)" % p,
                  file=sys.stderr)
    try:
        v = subprocess.run(shlex.split(a.version_cmd), stdout=subprocess.PIPE,
                           stderr=subprocess.STDOUT, timeout=120)
        info["tesseract_version"] = v.stdout.decode("utf-8", "replace").strip()
    except Exception as e:
        info["tesseract_version"] = "unknown (%s)" % e

    done = done_keys(a.out)
    side = a.out + ".meta.json"
    if done:                            # resuming: provenance must match
        old, problems = None, []
        try:
            with open(side, encoding="utf-8") as f:
                old = json.load(f)
        except (OSError, ValueError) as e:
            problems.append("sidecar %s unreadable (%s)" % (side, e))
        if old is not None:
            problems += resume_problems(old, info, {k[2] for k in done})
        if problems:
            print("REFUSING to resume %s: %s\nUse a new --out (or restore the "
                  "original model, command and tesseract)." % (a.out, "; ".join(problems)),
                  file=sys.stderr)
            return 2
        info["arms"] = {**old["arms"], **info["arms"]}

    jobs, queued, missing_all = [], set(), {}
    expected = {arm: {} for arm in arms}
    for s in a.sets.split(","):
        d = os.path.join(a.data_root, s)
        meta = dict(load_meta(os.path.join(d, "meta.csv"))) \
            if os.path.exists(os.path.join(d, "meta.csv")) else {}
        meta.update(meta_all)           # --meta wins over <set>/meta.csv
        pages, skipped, missing = find_pages(s, d, meta, (y0, y1), a.keep_unknown_year)
        if missing:
            missing_all[s] = sorted(missing)
        if a.limit:
            pages = pages[:a.limit]
        print("%s: %d pages in %d-%d (%d skipped by year)" % (s, len(pages), y0, y1,
                                                             skipped), file=sys.stderr)
        for arm in arms:
            expected[arm][s] = sorted({pg["page_id"] for pg in pages})
        for pg in pages:
            for arm in arms:
                k = (s, pg["page_id"], arm)
                if k not in done and k not in queued:
                    queued.add(k)
                    jobs.append((pg, arm, a.cmd, a.tessdata, a.timeout, gpu,
                                 a.cpu_cgroup))
    if done:        # keep earlier expectations and unresolved missing images
        for arm, sets in (old.get("expected") or {}).items():
            expected.setdefault(arm, {})
            for s, ids in sets.items():
                expected[arm][s] = sorted(set(ids) | set(expected[arm].get(s, [])))
        found = {}
        for sets in expected.values():
            for s, ids in sets.items():
                found.setdefault(s, set()).update(ids)
        for s, ids in (old.get("missing_images") or {}).items():
            keep = set(ids) - found.get(s, set())
            if keep:
                missing_all[s] = sorted(keep | set(missing_all.get(s, [])))
    if missing_all:
        listing = "; ".join("%s: %s" % (k, ", ".join(v)) for k, v in sorted(missing_all.items()))
        if not a.allow_missing_images:
            print("REFUSING to score: %d year-eligible pages have no image (%s). "
                  "Fetch the images or pass --allow-missing-images (the run is then "
                  "INCOMPLETE in analyze.py)." % (
                      sum(len(v) for v in missing_all.values()), listing), file=sys.stderr)
            return 2
        print("WARNING: scoring without images for: " + listing, file=sys.stderr)
    info["missing_images"] = missing_all
    info["expected"] = expected         # eligible page ids per arm and set
    with open(side, "w", encoding="utf-8") as f:
        json.dump(info, f, indent=1, sort_keys=True)
    new = not os.path.exists(a.out) or os.path.getsize(a.out) == 0
    n_ok = n_fail = 0
    with open(a.out, "a", newline="", encoding="utf-8") as f:
        w = csv.DictWriter(f, FIELDS)
        if new:
            w.writeheader()
        it = (map(job, jobs) if a.workers <= 1 else
              mp.Pool(a.workers).imap_unordered(job, jobs))
        for row, err in it:
            if row:
                w.writerow(row); f.flush(); n_ok += 1
            else:
                print("FAILED", err, file=sys.stderr); n_fail += 1
    print("done: %d rows, %d failed" % (n_ok, n_fail), file=sys.stderr)
    return 1 if n_fail else 0

if __name__ == "__main__":
    sys.exit(main())
