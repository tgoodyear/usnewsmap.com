"""How good LoC's OCR text is, by language and decade, from a sample of pages.

    python3 jaocr.py quality [--sample-pct 2] [--min-pages 50]

It reads every curated Parquet part of every batch in the published version
once, keeps a fixed sample of pages (a page is in it when a hash of its
doc_id, mod 10,000, is below sample_pct x 100, so a rerun takes the same
pages), and scores each sampled page's stored text:

- dict_share: of its word tokens (letters only once surrounding punctuation
  is stripped, at least 2 letters, case-folded), the share in a word list of
  the page's language (wordfreq's top 200,000 words). The page's language is
  the first language its title lists in the catalog; `multilingual` when the
  title lists more than one. Languages wordfreq has no list for get none.
  Modern word lists miss historical spellings (German "Sclaverei"), so old
  text that is read correctly can still score below 1.
- garbage_share: the share of its whitespace tokens that rmgarbage-style
  rules (Taghva et al.) flag: longer than 40 characters; more than half not
  letters or digits; the same letter 3 times in a row; Latin letters only,
  4 or more, with no vowel or nothing but vowels; mixed case inside the word
  ("tHe", not "McKay").

Pages of titles that list Japanese are counted on their own and not scored:
our own OCR covers those (04 section 4.8).

It writes audit/ocr-quality-<version>-<pct>pct.json (every table) and one
CSV per table next to it in the curated store, and logs each row of the
language and decade tables and each worst title and batch as an "ocr
quality" line, then a summary. A lock (as audit.py's) keeps a second replica
of the job out; a rerun overwrites the output.
"""

from __future__ import annotations

import csv
import hashlib
import io
import json
import multiprocessing
import os
import re
import statistics
import time
from array import array
from concurrent.futures import FIRST_COMPLETED, ProcessPoolExecutor, wait
from dataclasses import dataclass, field
from datetime import datetime, timedelta, timezone

import jaocr

COLUMNS = ["doc_id", "lccn", "date", "text_status", "text"]
WORDLIST_SIZE = 200_000
POOR, FAIR = 0.5, 0.7
WORST_TITLES, WORST_BATCHES = 100, 50
PROGRESS_BATCHES = 50
RENEW_EVERY = timedelta(minutes=5)

# The catalog's MARC codes to wordfreq's (ISO 639-1, "sh" for Serbo-Croatian).
# Only languages wordfreq 3.1 has a list for: it would otherwise fall back to
# the "nearest" one (Yiddish to English, Latin to Italian). Chinese and Korean
# are left out: their lists hold segmented words, and LoC's OCR of those
# pages is Latin letters anyway.
WORDFREQ = {
    "ara": "ar", "bul": "bg", "cze": "cs", "dan": "da", "dut": "nl", "eng": "en", "fin": "fi",
    "fre": "fr", "ger": "de", "gre": "el", "heb": "he", "hrv": "sh", "hun": "hu", "ice": "is",
    "ita": "it", "lav": "lv", "lit": "lt", "nor": "nb", "pol": "pl", "por": "pt", "rum": "ro",
    "rus": "ru", "slo": "sk", "slv": "sl", "spa": "es", "srp": "sh", "swe": "sv", "tgl": "fil",
    "ukr": "uk",
}
# Languages with words of no vowel (Czech "smrt", Croatian "prst"): the
# no-vowel rule would flag real words.
SYLLABIC_CONSONANTS = {"cze", "slo", "hrv", "srp", "slv"}
JAPANESE = "jpn"
UNKNOWN = "und"

CORE = re.compile(r"[^\W_](?:.*[^\W_])?", re.S)  # first to last letter or digit
TRIPLE = re.compile(r"([^\W\d_])\1\1")
ROMAN = re.compile(r"^(?=[mdclxvi]+$)m{0,4}(cm|cd|d?c{0,3})(xc|xl|l?x{0,3})(ix|iv|v?i{0,3})$")
VOWELS = frozenset("aeiouyàáâãäåæèéêëìíîïòóôõöøùúûüýÿāăąēĕėęěīĭįıōŏőœūŭůűų")


# ---------------------------------------------------------------- per page


def sampled(doc_id: str, cut: int) -> bool:
    """Whether a page is in the sample: a stable hash of its doc_id, mod 10,000, below `cut`."""
    h = int.from_bytes(hashlib.blake2b(doc_id.encode(), digest_size=8).digest(), "big")
    return h % 10_000 < cut


def latin(word: str) -> bool:
    return all(ord(c) < 0x250 or 0x1E00 <= ord(c) <= 0x1EFF for c in word)


def mixed_case(word: str) -> bool:
    """A capital inside a lower-case word: "tHe", "ThE", "HeLLoS"; not "McKay", "MacDonald", "USA"."""
    if word.islower() or word.isupper() or len(word) < 2:
        return False
    if word[0].islower():
        return True
    rises = sum(1 for a, b in zip(word, word[1:]) if a.islower() and b.isupper())
    return rises >= 2 or (rises == 1 and word[-1].isupper() and word[-2].islower())


def garbage(token: str, core: str | None, lang: str) -> bool:
    """rmgarbage-style rules (Taghva, Nartker, Condit and Borsack 2001), on a whitespace token."""
    if len(token) > 40:
        return True
    if len(token) >= 2 and sum(1 for c in token if not c.isalnum()) * 2 > len(token):
        return True
    if not core:
        return False
    folded = core.casefold()
    if TRIPLE.search(folded) and not ROMAN.match(folded):
        return True
    if core.isalpha():
        if len(core) >= 4 and latin(core):
            vowels = sum(1 for c in folded if c in VOWELS)
            if vowels == len(folded) or (vowels == 0 and lang not in SYLLABIC_CONSONANTS):
                return True
        if mixed_case(core):
            return True
    return False


@dataclass
class PageScore:
    dict_share: float | None
    garbage_share: float | None
    chars: int
    tokens: int
    words: int


def score(text: str, words: frozenset | None, lang: str) -> PageScore:
    """dict_share (None without a word list or word tokens), garbage_share, chars, tokens, words."""
    tokens = text.split()
    flagged = known = n_words = 0
    for tok in tokens:
        m = CORE.search(tok)
        core = m.group(0) if m else None
        if garbage(tok, core, lang):
            flagged += 1
        if core and len(core) >= 2 and core.isalpha():
            n_words += 1
            if words is not None and core.casefold() in words:
                known += 1
    return PageScore(
        dict_share=known / n_words if words is not None and n_words else None,
        garbage_share=flagged / len(tokens) if tokens else None,
        chars=len(text), tokens=len(tokens), words=n_words,
    )


# ---------------------------------------------------------------- languages


def title_language(title: dict | None) -> tuple[str, bool]:
    """The page's language (the title's first catalog language) and whether the title lists more."""
    langs = (title or {}).get("languages") or []
    return (langs[0] if langs else UNKNOWN), len(langs) > 1


def wordlist_code(lang: str) -> str | None:
    return WORDFREQ.get(lang)


def wordfreq_words(code: str) -> frozenset | None:
    """wordfreq's top words for a language, or None if it has no list of its own."""
    import wordfreq

    if code not in wordfreq.available_languages("best"):
        return None
    words = frozenset(wordfreq.top_n_list(code, WORDLIST_SIZE))
    # top_n_list and the frequency list it reads are cached; the set is all we need.
    wordfreq.top_n_list.cache_clear()
    wordfreq.get_frequency_list.cache_clear()
    return words


class WordLists:
    """Word lists by wordfreq code, each loaded once. `loader` is replaceable for tests."""

    def __init__(self, loader=None):
        self.loader = loader or wordfreq_words
        self.lists: dict[str, frozenset | None] = {}

    def get(self, code: str | None) -> frozenset | None:
        if code is None:
            return None
        if code not in self.lists:
            self.lists[code] = self.loader(code)
        return self.lists[code]


# ---------------------------------------------------------------- per part (in a worker)


@dataclass
class TitleInfo:
    lang: str
    multilingual: bool
    wordlist: str | None
    japanese: bool


_W: dict = {}


def init_worker(curated, titles: dict[str, TitleInfo], cut: int, loader=None) -> None:
    _W.update(curated=curated, titles=titles, cut=cut, words=WordLists(loader))


def scan_part(path: str) -> dict:
    """Read one curated part and score its sampled pages: {pages_read, pages: [...]} or {error}."""
    import pyarrow as pa
    import pyarrow.parquet as pq

    titles, cut, lists = _W["titles"], _W["cut"], _W["words"]
    try:
        f = pq.ParquetFile(io.BytesIO(_W["curated"].read(path)))
        read, out = 0, []
        for rb in f.iter_batches(batch_size=4096, columns=COLUMNS):
            ids = rb.column("doc_id").to_pylist()
            read += len(ids)
            keep = [sampled(d, cut) for d in ids]
            if not any(keep):
                continue
            for r in rb.filter(pa.array(keep)).to_pylist():
                t = titles.get(r["lccn"])
                status = r["text_status"]
                page = {"lccn": r["lccn"], "decade": r["date"].year // 10 * 10, "status": status}
                if status == "ok" and r["text"] and not (t and t.japanese):
                    lang = t.lang if t else UNKNOWN
                    s = score(r["text"], lists.get(t.wordlist if t else None), lang)
                    page.update(dict_share=s.dict_share, garbage_share=s.garbage_share,
                                chars=s.chars, tokens=s.tokens)
                out.append(page)
        return {"pages_read": read, "pages": out}
    except Exception as e:  # noqa: BLE001 - one part mustn't end the audit
        return {"error": f"{type(e).__name__}: {e}"[:300]}


# ---------------------------------------------------------------- aggregation


@dataclass
class Stats:
    pages: int = 0
    empty: int = 0
    short: int = 0
    multilingual: int = 0
    dict_shares: array = field(default_factory=lambda: array("f"))
    garbage_sum: float = 0.0
    garbage_n: int = 0
    chars: int = 0
    tokens: int = 0
    scored_text: int = 0  # ok pages with text that were scored

    def add(self, page: dict, multilingual: bool) -> None:
        self.pages += 1
        self.multilingual += multilingual
        if page["status"] == "empty":
            self.empty += 1
        elif page["status"] == "short":
            self.short += 1
        if "chars" in page:
            self.scored_text += 1
            self.chars += page["chars"]
            self.tokens += page["tokens"]
            if page["garbage_share"] is not None:
                self.garbage_sum += page["garbage_share"]
                self.garbage_n += 1
            if page["dict_share"] is not None:
                self.dict_shares.append(page["dict_share"])

    def merge(self, other: "Stats") -> "Stats":
        self.pages += other.pages
        self.empty += other.empty
        self.short += other.short
        self.multilingual += other.multilingual
        self.dict_shares.extend(other.dict_shares)
        self.garbage_sum += other.garbage_sum
        self.garbage_n += other.garbage_n
        self.chars += other.chars
        self.tokens += other.tokens
        self.scored_text += other.scored_text
        return self

    def row(self) -> dict:
        d = self.dict_shares
        n = len(d)

        def r(x):
            return None if x is None else round(x, 4)

        return {
            "pages": self.pages,
            "multilingual_pages": self.multilingual,
            "empty_share": r(self.empty / self.pages) if self.pages else None,
            "short_share": r(self.short / self.pages) if self.pages else None,
            "scored_pages": n,
            "dict_share_mean": r(sum(d) / n) if n else None,
            "dict_share_median": r(statistics.median(d)) if n else None,
            "poor_share": r(sum(1 for x in d if x < POOR) / n) if n else None,
            "fair_share": r(sum(1 for x in d if x < FAIR) / n) if n else None,
            "garbage_share_mean": r(self.garbage_sum / self.garbage_n) if self.garbage_n else None,
            "chars_mean": round(self.chars / self.scored_text) if self.scored_text else None,
            "tokens_mean": round(self.tokens / self.scored_text) if self.scored_text else None,
        }


class Tally:
    """Everything the report needs, from the sampled pages, one part at a time."""

    def __init__(self, titles: dict[str, TitleInfo]):
        self.titles = titles
        self.by_lang_decade: dict[tuple[str, int], Stats] = {}
        self.by_title: dict[str, Stats] = {}
        self.by_batch: dict[str, Stats] = {}
        self.batch_langs: dict[str, dict[str, int]] = {}
        self.japanese = Stats()
        self.untitled = 0

    def add(self, batch: str, pages: list[dict]) -> None:
        for p in pages:
            t = self.titles.get(p["lccn"])
            if t is None:
                self.untitled += 1
            if t and t.japanese:
                self.japanese.add(p, t.multilingual)
                continue
            lang, multi = (t.lang, t.multilingual) if t else (UNKNOWN, False)
            self.by_lang_decade.setdefault((lang, p["decade"]), Stats()).add(p, multi)
            if t and t.wordlist:
                self.by_title.setdefault(p["lccn"], Stats()).add(p, multi)
                self.by_batch.setdefault(batch, Stats()).add(p, multi)
                langs = self.batch_langs.setdefault(batch, {})
                langs[lang] = langs.get(lang, 0) + 1

    def tables(self, catalog: dict[str, dict], min_pages: int) -> dict[str, list[dict]]:
        by_lang: dict[str, Stats] = {}
        by_decade: dict[int, Stats] = {}
        for (lang, decade), s in self.by_lang_decade.items():
            by_lang.setdefault(lang, Stats()).merge(s)
            by_decade.setdefault(decade, Stats()).merge(s)

        def lrow(lang, decade, s):
            return {"language": lang, "wordlist": wordlist_code(lang) if lang != "all" else None,
                    "decade": decade, **s.row()}

        lang_decade = [lrow(lang, dec, s) for (lang, dec), s in
                       sorted(self.by_lang_decade.items(), key=lambda kv: (-by_lang[kv[0][0]].pages, kv[0]))]
        languages = [lrow(lang, "all", s) for lang, s in sorted(by_lang.items(), key=lambda kv: (-kv[1].pages, kv[0]))]
        decades = [lrow("all", dec, s) for dec, s in sorted(by_decade.items())]

        def worst(groups: dict[str, Stats], n: int):
            ok = [(k, s.row()) for k, s in groups.items() if len(s.dict_shares) >= max(min_pages, 1)]
            ok.sort(key=lambda kv: (kv[1]["dict_share_median"], kv[0]))
            return ok[:n]

        titles = []
        for rank, (lccn, row) in enumerate(worst(self.by_title, WORST_TITLES), 1):
            c, t = catalog.get(lccn, {}), self.titles[lccn]
            titles.append({"rank": rank, "lccn": lccn, "name": c.get("name", ""), "language": t.lang,
                           "languages": " ".join(c.get("languages") or []), "multilingual": t.multilingual,
                           "place": c.get("place_of_publication") or c.get("place_id", ""),
                           "state": c.get("state", ""), **row})
        batches = []
        for rank, (batch, row) in enumerate(worst(self.by_batch, WORST_BATCHES), 1):
            mix = sorted(self.batch_langs[batch].items(), key=lambda kv: (-kv[1], kv[0]))
            batches.append({"rank": rank, "batch": batch,
                            "languages": " ".join(f"{k}:{v}" for k, v in mix), **row})
        return {"by_language_decade": lang_decade, "by_language": languages, "by_decade": decades,
                "worst_titles": titles, "worst_batches": batches}


# ---------------------------------------------------------------- the run


def title_infos(titles: list[dict]) -> dict[str, TitleInfo]:
    out = {}
    for t in titles:
        lang, multi = title_language(t)
        out[t["lccn"]] = TitleInfo(lang=lang, multilingual=multi, wordlist=wordlist_code(lang),
                                   japanese=JAPANESE in (t.get("languages") or []))
    return out


def load_titles(reference, version: str) -> dict[str, dict]:
    """The published titles, plus the working catalog's for any it lacks (as jaocr.targets reads them)."""
    by_lccn = {t["lccn"]: t for t in json.loads(reference.read(f"{version}/titles.json"))}
    if reference.exists("catalog/titles.json"):
        for t in json.loads(reference.read("catalog/titles.json")):
            by_lccn.setdefault(t["lccn"], t)
    return by_lccn


def batch_name(b: dict) -> str:
    v = (b.get("curated") or {}).get("version")
    return f"{b['batch']}_ver{v:02d}" if isinstance(v, int) else b["batch"]


def pct_label(pct: float) -> str:
    return f"{pct:g}"


def results(tasks: list[tuple[str, str]], workers: int, initargs: tuple):
    """(batch, part, result) for each task, in completion order, with at most 2 x workers parts in flight."""
    if workers <= 1:
        init_worker(*initargs)
        for batch, part in tasks:
            yield batch, part, scan_part(part)
        return
    # Spawned, not forked: each worker unpickles the store and opens its own
    # blob client, rather than sharing the parent's connections.
    ctx = multiprocessing.get_context("spawn")
    with ProcessPoolExecutor(max_workers=workers, mp_context=ctx, initializer=init_worker,
                             initargs=initargs) as ex:
        pending: dict = {}
        it = iter(tasks)
        try:
            while True:
                while len(pending) < 2 * workers:
                    task = next(it, None)
                    if task is None:
                        break
                    pending[ex.submit(scan_part, task[1])] = task
                if not pending:
                    return
                done, _ = wait(pending, return_when=FIRST_COMPLETED)
                for fut in done:
                    batch, part = pending.pop(fut)
                    yield batch, part, fut.result()
        finally:
            ex.shutdown(cancel_futures=True)


def write_csv(rows: list[dict]) -> bytes:
    buf = io.StringIO()
    w = csv.DictWriter(buf, fieldnames=list(rows[0]) if rows else ["empty"])
    w.writeheader()
    w.writerows(rows)
    return buf.getvalue().encode()


def quality(reference, curated, sample_pct: float = 2.0, min_pages: int = 50,
            workers: int | None = None, loader=None) -> dict | None:
    if not 0 < sample_pct <= 100:
        raise ValueError(f"--sample-pct must be above 0 and at most 100, not {sample_pct}")
    cut = round(sample_pct * 100)
    if cut < 1:
        raise ValueError(f"--sample-pct {sample_pct} samples nothing: use at least 0.01")
    current = json.loads(reference.read("current.json"))
    version = current["reference"]
    if not jaocr.SAFE_SEGMENT.match(version):
        raise ValueError(f"current.json names an unsafe reference version: {version!r}")
    base = f"audit/ocr-quality-{version}-{pct_label(sample_pct)}pct"

    # The job runs several replicas: one does the work. As in audit.py, the
    # lock's owner renews it as it goes, and a lock not renewed for
    # JAOCR_AUDIT_LOCK_MINUTES (60) is taken over.
    lock = f"{base}.lock"
    owner = os.environ.get("CONTAINER_APP_REPLICA_NAME") or os.environ.get("HOSTNAME") or "local"

    def lease() -> bytes:
        return json.dumps({"owner": owner, "at": datetime.now(timezone.utc).isoformat()}).encode()

    stale = timedelta(minutes=float(os.environ.get("JAOCR_AUDIT_LOCK_MINUTES", "60")))
    if not (curated.create(lock, lease()) or curated.take_over(lock, lease(), stale)):
        jaocr.log("ocr quality running in another replica", version=version, lock=lock)
        return None

    catalog = load_titles(reference, version)
    titles = title_infos(list(catalog.values()))
    batches = json.loads(reference.read(f"{version}/batches.json"))
    tasks = [(batch_name(b), p) for b in batches for p in (b.get("curated") or {}).get("parts") or []]
    parts_left: dict[str, int] = {}
    for name, _ in tasks:
        parts_left[name] = parts_left.get(name, 0) + 1
    if workers is None:
        workers = int(os.environ.get("JAOCR_QUALITY_WORKERS", "4"))
    jaocr.log("ocr quality starting", version=version, sample_pct=sample_pct, min_pages=min_pages,
              batches=len(batches), parts=len(tasks), titles=len(titles), workers=workers)

    tally = Tally(titles)
    started = time.time()
    last_renew = datetime.now(timezone.utc)
    pages_read = pages_sampled = parts_failed = 0
    batches_done = len(batches) - len(parts_left)  # batches with no parts
    for batch, part, res in results(tasks, workers, (curated, titles, cut, loader)):
        if "error" in res:
            parts_failed += 1
            jaocr.log("ocr quality part failed", batch=batch, part=part, error=res["error"])
        else:
            pages_read += res["pages_read"]
            pages_sampled += len(res["pages"])
            tally.add(batch, res["pages"])
        parts_left[batch] -= 1
        if parts_left[batch] == 0:
            batches_done += 1
            if batches_done % PROGRESS_BATCHES == 0:
                jaocr.log("ocr quality progress", batches_done=batches_done, batches=len(batches),
                          pages_read=pages_read, pages_sampled=pages_sampled, parts_failed=parts_failed,
                          elapsed_s=round(time.time() - started))
        if datetime.now(timezone.utc) - last_renew >= RENEW_EVERY:
            if not curated.renew(lock, lease(), owner):
                jaocr.log("ocr quality lock lost; stopping", version=version, batches_done=batches_done)
                return None
            last_renew = datetime.now(timezone.utc)

    tables = tally.tables(catalog, min_pages)
    japanese = {"language": JAPANESE, **tally.japanese.row()}
    summary = {
        "version": version, "sample_pct": sample_pct, "min_pages": min_pages,
        "batches": len(batches), "parts": len(tasks), "parts_failed": parts_failed,
        "pages_read": pages_read, "pages_sampled": pages_sampled,
        "japanese_pages_skipped": tally.japanese.pages, "pages_without_title": tally.untitled,
        "elapsed_s": round(time.time() - started),
    }
    report = {"summary": summary, "generated_at": datetime.now(timezone.utc).isoformat(),
              "wordlists": {"source": "wordfreq top_n_list", "size": WORDLIST_SIZE,
                            "codes": WORDFREQ, "version": _wordfreq_version()},
              "japanese_skipped": japanese, **tables}
    curated.write(f"{base}.json", json.dumps(report, ensure_ascii=False, indent=1).encode())
    paths = [f"{base}.json"]
    for name, rows in tables.items():
        path = f"{base}-{name.replace('_', '-')}.csv"
        curated.write(path, write_csv(rows))
        paths.append(path)

    for name in ("by_language", "by_decade", "by_language_decade"):
        for r in tables[name]:
            jaocr.log("ocr quality", table=name, version=version, **r)
    jaocr.log("ocr quality", table="japanese_skipped", version=version, **japanese)
    for name in ("worst_titles", "worst_batches"):
        for r in tables[name]:
            jaocr.log("ocr quality", table=name, version=version, **r)
    jaocr.log("ocr quality finished", **summary, outputs=paths)
    return report


def _wordfreq_version() -> str | None:
    try:
        from importlib.metadata import version

        return version("wordfreq")
    except Exception:  # noqa: BLE001 - only for the report
        return None
