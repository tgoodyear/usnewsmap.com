"""Recompute `mark_only` and `lookalikes` on check.usnewsmap.com task bodies that are already loaded (#1939).

Tasks loaded before those fields existed, or before their rules changed, carry both readings and need no images:
each stored body has `a` and `b`. This tool runs `lookalike.analyse`, the call build_tasks.py makes for these two
fields, on every body of a language, with the language's `equivalents` from languages/<lang>.json. It writes the
new bodies and a report of what would change. It changes nothing on the site unless run with --action load.

A new body differs from the old one only in `mark_only` and `lookalikes`. The body keeps its key order: a field it
already had is replaced in place, a field it gains goes after the last key (`lookalikes` before `mark_only`, where
build_tasks.py writes them: the existing bodies are in build order, not sorted), and a field that no longer applies
is dropped. Every other key, in its place, with its value, must be equal; a difference fails the run and writes no
bodies. Ids, `lang` and `set` are kept.

Input, one of (--action dry-run):
  --bodies FILE   JSON lines of {"id", "lang", "set", ...body}: the form /admin/tasks takes, and tasks.jsonl
  --dump FILE     a /admin/dump response (the `tasks` rows hold the bodies)
  --vault NAME    read /admin/dump from --site with the admin token from that Key Vault (the token is kept in memory,
                  never on a command line). Read-only.
  --export FILE   (repeatable) the public export (`/api/export?lang=...`). It has no full bodies, so this only previews: the report
                  says what would change and no bodies are written, so it can't be loaded.

Output, in --out-dir/<lang>/: tasks.jsonl (the changed tasks' new bodies, in the /admin/tasks form), report.json
(totals, per-task changes, the SHA-256 of the input bodies and of tasks.jsonl, `passed`) and report.txt.

Loading (--action load) reads OUT/<lang>/report.json and refuses unless it passed, was made from full bodies, and its
hash matches the tasks.jsonl on disk. It then reads the current bodies from the site again, refuses if they are not
the ones the dry run saw, checks each new body against its current one again, sends the changed tasks to
POST /admin/tasks, reads the bodies back and checks they are the new ones. /admin/tasks replaces a task's body and
touches nothing else (README.md, "Updating loaded tasks").

Without Raqm, a language whose readings contain combining marks is refused: those letters can't be drawn the way the
browser draws them. A letter pair with no font or glyph fails the run unless --allow-unmeasured. Cherokee is always
measured in the font the site serves; --font CHEROKEE=PATH is refused.

  python3 recompute_marks.py --lang yi --lang sr --vault NAME --out-dir OUT
  python3 recompute_marks.py --action load --lang yi --vault NAME --out-dir OUT
"""
import argparse
import collections
import hashlib
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request
import urllib.parse
import unicodedata

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import build_tasks as bt  # noqa: E402
import lookalike as la  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
FIELDS = ("lookalikes", "mark_only")  # the order build_tasks.py writes them in
TOKEN_SECRET = "usnm-prod-check-admin-token"
DEFAULT_SITE = "https://check.usnewsmap.com"
CHUNK = 200


class Refused(Exception):
    """The run can't produce bodies that are safe to load."""


def canonical(record: dict) -> str:
    return json.dumps(record, ensure_ascii=False, separators=(",", ":"))


def sha256(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def input_hash(records: list) -> str:
    """Hash of a language's bodies as read, in id order: what the dry run saw and what a load must find again."""
    return sha256("\n".join(canonical(r) for r in sorted(records, key=lambda r: r["id"])))


# ---------------------------------------------------------------- reading

def read_bodies(path: str) -> list:
    out = []
    for raw in read_text(path).splitlines():
        if raw.strip():
            r = json.loads(raw)
            if not all(isinstance(r.get(k), str) for k in ("id", "lang", "set")):
                raise Refused(f"{path}: every line needs id, lang and set")
            out.append(r)
    return out


def records_from_dump(text: str) -> list:
    out = []
    for raw in text.splitlines():
        if not raw.strip():
            continue
        line = json.loads(raw)
        if line.get("table") != "tasks":
            continue
        row = line["row"]
        out.append({"id": row["id"], "lang": row["lang"], "set": row["set_name"], **json.loads(row["body"])})
    return out


def read_export(paths: list) -> list:
    """Export rows as preview records: id, lang, set, a, b and the two fields as exported. Not full bodies."""
    out = []
    for raw in (l for p in paths for l in read_text(p).splitlines()):
        if not raw.strip():
            continue
        t = json.loads(raw)
        r = {"id": t["id"], "lang": t["id"].split("/")[0], "set": t.get("set"), "a": t["a"], "b": t["b"]}
        for f in FIELDS:
            if f in t:
                r[f] = t[f]
        out.append(r)
    return out


class Site:
    """The admin routes this tool uses: GET /admin/dump (read) and POST /admin/tasks (load). The token stays in memory."""

    def __init__(self, base: str, token: str):
        parsed = urllib.parse.urlparse(base)
        local = parsed.scheme == "http" and parsed.hostname in ("localhost", "127.0.0.1", "::1")
        if not (parsed.scheme == "https" or local) or not parsed.hostname or parsed.path not in ("", "/"):
            raise Refused(f"--site wants an https origin, got {base!r}")
        self.base = f"{parsed.scheme}://{parsed.netloc}"
        self._token = token

    def _request(self, path: str, data: bytes = None, timeout: int = 300) -> bytes:
        req = urllib.request.Request(self.base + path, data=data, method="POST" if data is not None else "GET",
                                     headers={"authorization": "Bearer " + self._token, "user-agent": "usnm-check-recompute/1"})
        try:
            with urllib.request.urlopen(req, timeout=timeout) as resp:
                return resp.read()
        except urllib.error.HTTPError as e:
            raise Refused(f"{path}: HTTP {e.code}") from None

    def dump_records(self) -> list:
        return records_from_dump(self._request("/admin/dump").decode("utf-8"))

    def post_tasks(self, lines: list) -> dict:
        return json.loads(self._request("/admin/tasks", ("\n".join(lines) + "\n").encode("utf-8")))


def vault_token(vault: str) -> str:
    if not vault or not all(c.isalnum() or c == "-" for c in vault):
        raise Refused("--vault wants a Key Vault name")
    out = subprocess.run(["az", "keyvault", "secret", "show", "--vault-name", vault, "-n", TOKEN_SECRET, "--query", "value", "-o", "tsv"],
                         capture_output=True, text=True)
    token = out.stdout.strip()
    if out.returncode or not token:
        raise Refused(f"no admin token in {vault}")
    return token


# ---------------------------------------------------------------- recomputing

def has_combining(text: str) -> bool:
    return any(unicodedata.category(ch).startswith("M") for ch in text)


def new_body(old: dict, alike: list, marks: list) -> dict:
    """`old` (a record or a body) with `lookalikes` and `mark_only` set to the new values and every other key as it was.
    A field already there is replaced in place; one that is gained goes at the end, `lookalikes` first; an empty one is dropped."""
    new = {}
    for k, v in old.items():
        if k in FIELDS:
            val = alike if k == "lookalikes" else marks
            if val:
                new[k] = val
        else:
            new[k] = v
    for k, val in (("lookalikes", alike), ("mark_only", marks)):
        if val and k not in new:
            new[k] = val
    return new


def other_fields(record: dict) -> list:
    return [(k, v) for k, v in record.items() if k not in FIELDS]


def check_only_fields_differ(old: dict, new: dict) -> str:
    """"" when `new` is `old` in everything but `lookalikes` and `mark_only` (same keys in the same order, equal
    values, so the same bytes once written), else what differs."""
    if old.get("id") != new.get("id"):
        return "id differs"
    a, b = other_fields(old), other_fields(new)
    if [k for k, _ in a] != [k for k, _ in b]:
        return f"keys differ: {[k for k, _ in a]} against {[k for k, _ in b]}"
    for (k, x), (_, y) in zip(a, b):
        if canonical(x) != canonical(y):
            return f"{k} differs"
    return ""


def changed_fields(old: dict, new: dict) -> list:
    return [f for f in FIELDS if old.get(f) != new.get(f)]


def kind(old, new) -> str:
    return "added" if not old and new else "removed" if old and not new else "changed"


class Context:
    def __init__(self, fonts, languages_dir, threshold, extra_equivalents, allow_unmeasured):
        self.fonts, self.languages_dir, self.threshold = fonts, languages_dir, threshold
        self.extra, self.allow_unmeasured = extra_equivalents, allow_unmeasured

    def equivalents(self, lang: str) -> dict:
        path = os.path.join(self.languages_dir, f"{lang}.json")
        if not os.path.exists(path):
            raise Refused(f"no language configuration {path}")
        cfg = json.loads(read_text(path))
        return bt.parse_equivalents([f"{k}={v}" for k, v in cfg.get("equivalents", {}).items()] + list(self.extra))


def recompute_language(lang: str, records: list, ctx: Context, full: bool) -> tuple:
    """(new records for the changed tasks, report). Raises Refused before any body is produced when the language can't
    be processed completely. `full`: the records are whole bodies (else an export preview)."""
    fonts = ctx.fonts
    if any(not isinstance(r.get("a"), str) or not isinstance(r.get("b"), str) for r in records):
        raise Refused(f"{lang}: a task has no readings `a` and `b`")
    if not fonts.raqm and any(has_combining(r["a"]) or has_combining(r["b"]) for r in records):
        raise Refused(f"{lang}: the readings have combining marks and this Pillow has no Raqm, so letter pairs with marks can't be "
                      "drawn as a browser draws them; run with a Pillow built with Raqm (its pip wheels have it)")
    eq = ctx.equivalents(lang)
    unmeasured0, unshaped0 = collections.Counter(fonts.unmeasured), collections.Counter(fonts.unshaped)
    changed_records, per_task = [], []
    by_field = collections.Counter()
    by_kind = collections.Counter()
    changed_by_mark = collections.Counter()
    pairs_by_mark = collections.Counter()
    diff_mismatch = []
    total_alike = total_marks = 0
    for rec in records:
        alike, marks = la.analyse(rec["a"], rec["b"], eq, fonts, ctx.threshold)
        total_alike += bool(alike)
        total_marks += bool(marks)
        for m in marks:
            for ch in la.differing_marks(m):
                pairs_by_mark[f"U+{ord(ch):04X}"] += 1
        if "a_diff" in rec and "b_diff" in rec:
            ad, bd = bt.letter_diff(rec["a"], rec["b"], eq)
            if (ad, bd) != (rec["a_diff"], rec["b_diff"]):
                diff_mismatch.append(rec["id"])
        new = new_body(rec, alike, marks)
        why = check_only_fields_differ(rec, new)
        if why:
            raise Refused(f"{rec['id']}: the new body differs in more than the two fields ({why})")
        ch = changed_fields(rec, new)
        if not ch:
            continue
        for f in ch:
            by_field[f] += 1
            by_kind[f"{f} {kind(rec.get(f), new.get(f))}"] += 1
        by_field["both"] += len(ch) == 2
        for m in (new.get("mark_only") or []) if "mark_only" in ch else []:
            for mark in la.differing_marks(m):
                changed_by_mark[f"U+{ord(mark):04X}"] += 1
        changed_records.append(new)
        per_task.append({"id": rec["id"], "changed": ch})
    unmeasured = {k: v - unmeasured0[k] for k, v in fonts.unmeasured.items() if v - unmeasured0[k]}
    unshaped = {k: v - unshaped0[k] for k, v in fonts.unshaped.items() if v - unshaped0[k]}
    if unshaped:
        raise Refused(f"{lang}: letter pairs with combining marks could not be shaped ({unshaped})")
    if unmeasured and not ctx.allow_unmeasured:
        raise Refused(f"{lang}: letter pairs with no font or no glyph were not measured ({unmeasured}); pass --font SCRIPT=PATH, or "
                      "--allow-unmeasured to accept them as unflagged, as build_tasks.py does")
    lines = [canonical(r) for r in changed_records]
    report = {
        "tool": "recompute_marks.py", "lang": lang, "passed": True, "preview": not full,
        "totals": {"tasks_read": len(records), "unchanged": len(records) - len(changed_records), "changed": len(changed_records),
                   "changed_by_field": {f: by_field[f] for f in FIELDS}, "changed_in_both_fields": by_field["both"],
                   "changed_by_kind": dict(sorted(by_kind.items())),
                   "changed_by_mark": dict(sorted(changed_by_mark.items())),
                   "tasks_with_lookalikes_after": total_alike, "tasks_with_mark_only_after": total_marks,
                   "mark_only_pairs_by_mark_after": dict(sorted(pairs_by_mark.items()))},
        "input_sha256": input_hash(records) if full else None,
        "tasks_sha256": sha256("".join(l + "\n" for l in lines)) if full else None,
        "threshold": ctx.threshold,
        "fonts": {s or "other": (os.path.basename(p) if p else None) for s, p in sorted(fonts.paths.items())},
        "raqm": fonts.raqm,
        "unmeasured_pairs": unmeasured,
        "letter_diff_mismatch": diff_mismatch,
        "tasks": per_task,
    }
    return changed_records, report


def report_text(rep: dict) -> str:
    t = rep["totals"]
    out = [f"{rep['lang']}: {t['tasks_read']} tasks read, {t['unchanged']} unchanged, {t['changed']} changed"
           + (" (preview from the public export: no bodies written, can't be loaded)" if rep["preview"] else ""),
           "  by field: " + ", ".join(f"{k} {v}" for k, v in t["changed_by_field"].items()) + f"; both {t['changed_in_both_fields']}",
           "  by kind: " + (", ".join(f"{k} {v}" for k, v in t["changed_by_kind"].items()) or "none"),
           "  changed tasks by mark: " + (", ".join(f"{k} {v}" for k, v in t["changed_by_mark"].items()) or "none"),
           f"  after: {t['tasks_with_lookalikes_after']} tasks with lookalikes, {t['tasks_with_mark_only_after']} with mark_only"]
    if rep["unmeasured_pairs"]:
        out.append(f"  letter pairs not measured (accepted): {rep['unmeasured_pairs']}")
    if rep["letter_diff_mismatch"]:
        out.append(f"  WARNING: {len(rep['letter_diff_mismatch'])} tasks' stored a_diff/b_diff differ from what this language configuration gives "
                   "(was the set built with other equivalents?): " + ", ".join(rep["letter_diff_mismatch"][:5]) + (" ..." if len(rep["letter_diff_mismatch"]) > 5 else ""))
    out.append("  fonts: " + ", ".join(f"{k}={v or 'none found'}" for k, v in rep["fonts"].items()) + f"; threshold {rep['threshold']}; Raqm {rep['raqm']}")
    out.append(f"  input sha256 {rep['input_sha256']}; tasks.jsonl sha256 {rep['tasks_sha256']}" if not rep["preview"] else "  hard check: only the two fields are recomputed (nothing else in an export to compare)")
    return "\n".join(out) + "\n"


# ---------------------------------------------------------------- the two actions

def write_json(path: str, data: dict) -> None:
    with open(path, "w", encoding="utf-8") as f:
        json.dump(data, f, ensure_ascii=False, indent=1, sort_keys=True)


def read_text(path: str) -> str:
    with open(path, encoding="utf-8") as f:
        return f.read()


def write_language(out_dir: str, lang: str, changed: list, rep: dict) -> None:
    d = os.path.join(out_dir, lang)
    os.makedirs(d, exist_ok=True)
    tasks_path = os.path.join(d, "tasks.jsonl")
    if rep["preview"]:
        if os.path.exists(tasks_path):
            os.remove(tasks_path)  # a preview must not leave an older language's bodies next to its report
    else:
        with open(tasks_path, "w", encoding="utf-8", newline="\n") as f:
            f.writelines(canonical(r) + "\n" for r in changed)
    write_json(os.path.join(d, "report.json"), rep)
    with open(os.path.join(d, "report.txt"), "w", encoding="utf-8") as f:
        f.write(report_text(rep))


def dry_run(a, ctx: Context, records: list, full: bool) -> int:
    failed = False
    by_lang = collections.defaultdict(list)
    for r in records:
        by_lang[r["lang"]].append(r)
    for lang in a.lang:
        recs = by_lang.get(lang, [])
        try:
            if not recs:
                raise Refused(f"{lang}: no tasks in the input")
            changed, rep = recompute_language(lang, recs, ctx, full)
        except Refused as e:
            print(f"REFUSED {e}", file=sys.stderr)
            d = os.path.join(a.out_dir, lang)
            os.makedirs(d, exist_ok=True)
            for name in ("tasks.jsonl", "report.json"):  # an older passed report must not satisfy a load
                if os.path.exists(os.path.join(d, name)):
                    os.remove(os.path.join(d, name))
            write_json(os.path.join(d, "report.json"), {"tool": "recompute_marks.py", "lang": lang, "passed": False, "reason": str(e)})
            failed = True
            continue
        write_language(a.out_dir, lang, changed, rep)
        print(report_text(rep), end="")
    return 1 if failed else 0


def load(a, site: Site) -> int:
    for lang in a.lang:
        d = os.path.join(a.out_dir, lang)
        try:
            rep = json.loads(read_text(os.path.join(d, "report.json")))
            lines = read_text(os.path.join(d, "tasks.jsonl"))
        except FileNotFoundError:
            raise Refused(f"{lang}: no dry-run report and tasks.jsonl in {d}; run the dry run first") from None
        if not rep.get("passed") or rep.get("preview") or rep.get("lang") != lang:
            raise Refused(f"{lang}: the dry-run report did not pass, or is a preview from the public export")
        if sha256(lines) != rep.get("tasks_sha256"):
            raise Refused(f"{lang}: tasks.jsonl is not the file the dry-run report passed (hash differs); run the dry run again")
        new = [json.loads(l) for l in lines.splitlines() if l.strip()]
        if len(new) != rep["totals"]["changed"] or [t["id"] for t in new] != [t["id"] for t in rep["tasks"]]:
            raise Refused(f"{lang}: tasks.jsonl does not match the report's task list")
        if not new:
            print(f"{lang}: nothing to load (no task changed)")
            continue
        current = [r for r in site.dump_records() if r["lang"] == lang]
        if input_hash(current) != rep["input_sha256"]:
            raise Refused(f"{lang}: the site's bodies are not the ones the dry run read (tasks were loaded or changed since); run the dry run again")
        old = {r["id"]: r for r in current}
        for t in new:
            if t["id"] not in old:
                raise Refused(f"{t['id']} is not a task on the site")
            if t["lang"] != lang or t["set"] != old[t["id"]]["set"]:
                raise Refused(f"{t['id']}: lang or set differs from the site's")
            why = check_only_fields_differ(old[t["id"]], t)
            if why:
                raise Refused(f"{t['id']}: the new body differs in more than the two fields ({why})")
        sent = 0
        for i in range(0, len(new), CHUNK):
            sent += site.post_tasks([canonical(t) for t in new[i:i + CHUNK]])["tasks"]
        after = {r["id"]: r for r in site.dump_records() if r["lang"] == lang}
        wrong = [t["id"] for t in new if after.get(t["id"]) != t]
        others = [i for i in old if i not in {t["id"] for t in new} and after.get(i) != old[i]]
        print(f"{lang}: sent {sent} bodies; read back {len(new) - len(wrong)} as sent, {len(wrong)} not; {len(others)} other tasks changed")
        if wrong or others:
            print("  check the site: " + ", ".join((wrong + others)[:5]), file=sys.stderr)
            return 1
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--action", choices=("dry-run", "load"), default="dry-run")
    ap.add_argument("--lang", action="append", required=True, help="a loaded language (yi, sr, chr, chr-lines, chr-rare); repeatable")
    ap.add_argument("--out-dir", required=True, help="where the dry run writes <lang>/tasks.jsonl and report.json, and a load reads them")
    ap.add_argument("--bodies", help="JSON lines of {id, lang, set, ...body}")
    ap.add_argument("--dump", help="a saved /admin/dump response")
    ap.add_argument("--export", action="append", help="the public export, one file per language (`/api/export?lang=...`); repeatable: preview only")
    ap.add_argument("--vault", help="Key Vault holding the admin token: read the bodies from the site (dry run) or load (load)")
    ap.add_argument("--site", default=DEFAULT_SITE)
    ap.add_argument("--languages-dir", default=os.path.join(HERE, "..", "languages"))
    ap.add_argument("--font", action="append", help="SCRIPT=PATH reference font (not for Cherokee, which uses the site's font); repeatable")
    ap.add_argument("--equivalent", action="append", default=[], help="LETTER=LETTERS added to the language's equivalents, as build_tasks.py's flag")
    ap.add_argument("--lookalike-threshold", type=float, default=la.THRESHOLD)
    ap.add_argument("--allow-unmeasured", action="store_true", help="accept letter pairs with no font or glyph as unflagged")
    a = ap.parse_args()
    try:
        if a.action == "load":
            if not a.vault:
                raise Refused("--action load needs --vault")
            return load(a, Site(a.site, vault_token(a.vault)))
        sources = [s for s in ("bodies", "dump", "export", "vault") if getattr(a, s)]
        if len(sources) != 1:
            raise Refused("give exactly one of --bodies, --dump, --export, --vault")
        overrides = la.parse_fonts(a.font)
        if "CHEROKEE" in overrides:
            raise Refused("Cherokee is measured in the font the site serves; --font CHEROKEE is not accepted")
        if not os.path.exists(la.BUNDLED["CHEROKEE"]):
            raise Refused("the bundled Cherokee font is missing")
        full = True
        if a.bodies:
            records = read_bodies(a.bodies)
        elif a.dump:
            records = records_from_dump(read_text(a.dump))
        elif a.export:
            records, full = read_export(a.export), False
        else:
            records = Site(a.site, vault_token(a.vault)).dump_records()
        ctx = Context(la.Fonts(overrides), a.languages_dir, a.lookalike_threshold, a.equivalent, a.allow_unmeasured)
        return dry_run(a, ctx, records, full)
    except Refused as e:
        print(f"REFUSED {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
