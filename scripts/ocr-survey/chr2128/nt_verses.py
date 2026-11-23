#!/usr/bin/env python3
"""Book, chapter and verse of each letter of `cherokee_testset.py`'s NT letter stream (#2128).

The aligner works on letter offsets into the NT's syllabary letters in reading order (`nt_stream`). The
silver set and R4 need verse ids, so that the new arms can leave the verses the silver set covers out of
training. This walks the same `pdftotext -raw` text line by line, with the same hyphen rejoining, and
tracks three kinds of line:
  - the next book's title (the 27 titles of the contents page, without their page numbers) on a line of
    its own or broken over two lines: a new book (also when chapter 1 starts after a later chapter);
  - `ᎠᏯᏙᎸᎢ N` (chapter N) on a line of its own: a new chapter;
  - a line starting with a number that follows the current verse (1 to 3 more): a new verse.
Running heads (`46 <title>` on even pages, `ᎠᏯᏙᎸᎢ <chapter> <page>` on odd ones) match none of these; their
letters are counted to the verse before them, which is what the aligner sees too.

A verse id is `<book number 01-27>.<chapter>.<verse>`; letters before a book's first verse get verse 0.

    nt_verses.py --nt-text cnts.raw.txt --out verses.json   (summary and the per-book chapter/verse counts)
"""
import argparse
import bisect
import json
import re

import common  # noqa: F401
import cherokee_testset as ct  # noqa: E402

CHAPTER = re.compile(r"^ᎠᏯᏙᎸᎢ (\d+)$")
VERSE = re.compile(r"^(\d+) ")
TOC_LINE = re.compile(r"^(.*[Ꭰ-Ᏽ].*?) (\d+)$")


class VerseMap:
    def __init__(self, text):
        text = re.sub(r"-\s*\n\s*", "", text)
        lines = text.split("\n")
        titles = []
        k = next(i for i, l in enumerate(lines) if l.strip() == "ᏗᎪᏪᎵ")  # the contents page
        for l in lines[k + 1:]:
            m = TOC_LINE.match(l.strip())
            if m:
                titles.append(m.group(1))
                if len(titles) == 27:
                    break
            elif titles and l.strip() not in ("v", ""):
                break
        self.titles = titles
        body = k + 1 + next(i for i, l in enumerate(lines[k + 1:]) if l.strip() == titles[0] and i > len(titles))
        n = 0
        for l in lines[:body]:
            n += sum(len(w) for w in ct.SYLLABARY.findall(l))
        self.starts, self.ids = [0], ["00.0.0"]
        book, chap, verse = 0, 0, 0
        rest = lines[body:]
        for j, l in enumerate(rest):
            s = l.strip()
            nxt = rest[j + 1].strip() if j + 1 < len(rest) else ""
            new = None
            want = titles[book] if book < len(titles) else None
            if want and (s == want or (want.startswith(s + " ") and f"{s} {nxt}".startswith(want))):
                book, chap, verse = book + 1, 0, 0
                new = f"{book:02d}.0.0"
            elif CHAPTER.match(s):
                c = int(CHAPTER.match(s).group(1))
                if c == 1 and chap >= 1 and want:  # a title the text breaks oddly: the chapter count restarts
                    book += 1
                chap, verse = c, 0
                new = f"{book:02d}.{chap}.0"
            else:
                m = VERSE.match(s)
                if m and chap and verse < int(m.group(1)) <= verse + 3 and ct.SYLLABARY.search(s):
                    verse = int(m.group(1))
                    new = f"{book:02d}.{chap}.{verse}"
            if new:
                self.starts.append(n)
                self.ids.append(new)
            n += sum(len(w) for w in ct.SYLLABARY.findall(l))
        self.letters = n

    def verses(self, a, b):
        """The verse ids of NT letters [a, b), in order."""
        i = bisect.bisect_right(self.starts, a) - 1
        out = []
        while i < len(self.starts) and (self.starts[i] < b or not out):
            out.append(self.ids[i])
            i += 1
        return [v for v in out if not v.endswith(".0")] or out


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--nt-text", required=True)
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    text = open(a.nt_text, encoding="utf-8").read()
    vm = VerseMap(text)
    nt, _ = ct.nt_stream(text)
    assert vm.letters == len(nt), (vm.letters, len(nt))
    books = {}
    for v in vm.ids:
        b, c, n = v.split(".")
        if n != "0":
            books.setdefault(b, {}).setdefault(c, 0)
            books[b][c] += 1
    summary = {"letters": vm.letters, "books": len(books), "chapters": sum(len(c) for c in books.values()),
               "verses": sum(sum(c.values()) for c in books.values()),
               "per_book": {b: {"chapters": len(c), "verses": sum(c.values())} for b, c in sorted(books.items())}}
    json.dump(summary, open(a.out, "w"), indent=1, sort_keys=True)
    print(json.dumps({k: summary[k] for k in ("letters", "books", "chapters", "verses")}))


if __name__ == "__main__":
    main()
