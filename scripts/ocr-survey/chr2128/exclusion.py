"""The verses no #2128 training arm may learn (Amendment 1, change 3): every verse any aligned GHN test line
touches, kept or dropped (`silver/verses-any-aligned.txt`, 274 verses).

Two checks, both used by the corpus builder, the synthetic-line writer and the R4c line builder:
  - by place: a verse id, or an NT letter offset, inside an excluded verse;
  - by text: a run of RUN syllabary letters (spaces ignored) that occurs in an excluded verse's letters.
    This catches the same verse reaching the training text by another road (ChrEn's parallel data is
    largely the same New Testament; the gospels repeat each other).
"""
import bisect

import common  # noqa: F401
import cherokee_testset as ct  # noqa: E402
import nt_verses  # noqa: E402

RUN = 20


def letters(s):
    return "".join(ct.SYLLABARY.findall(s))


class Exclusion:
    def __init__(self, nt_text_path, verses_path, run=RUN):
        text = open(nt_text_path, encoding="utf-8").read()
        self.vm = nt_verses.VerseMap(text)
        self.nt, _ = ct.nt_stream(text)
        self.run = run
        self.ids = {v.strip() for v in open(verses_path) if v.strip()}
        unknown = self.ids - set(self.vm.ids)
        assert not unknown, f"excluded verse ids not in the verse map: {sorted(unknown)[:5]}"
        self.spans = []
        for i, v in enumerate(self.vm.ids):
            if v in self.ids:
                end = self.vm.starts[i + 1] if i + 1 < len(self.vm.starts) else self.vm.letters
                self.spans.append((self.vm.starts[i], end))
        self.excluded_letters = sum(b - a for a, b in self.spans)
        self.grams = set()
        for a, b in self.spans:
            s = self.nt[a:b]
            for k in range(len(s) - run + 1):
                self.grams.add(s[k:k + run])

    def verse_of(self, offset):
        return self.vm.ids[bisect.bisect_right(self.vm.starts, offset) - 1]

    def offset_excluded(self, offset):
        return self.verse_of(offset) in self.ids

    def text_excluded(self, s):
        """Whether `s` holds a run of RUN letters found in an excluded verse."""
        t = letters(s)
        return any(t[k:k + self.run] in self.grams for k in range(len(t) - self.run + 1))


def nt_verse_words(nt_text_path, vm):
    """[(verse id, [words], first letter offset)] over the NT letter stream, in order."""
    import re
    text = re.sub(r"-\s*\n\s*", "", open(nt_text_path, encoding="utf-8").read())
    out, n = [], 0
    for w in ct.SYLLABARY.findall(text):
        v = vm.ids[bisect.bisect_right(vm.starts, n) - 1]
        if not out or out[-1][0] != v:
            out.append((v, [], n))
        out[-1][1].append(w)
        n += len(w)
    return out
