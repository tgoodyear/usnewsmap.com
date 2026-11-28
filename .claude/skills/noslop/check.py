#!/usr/bin/env python3
"""Mechanical sweep for the noslop skill.

Finds what eyes miss: em and en dashes, semicolons, stock vocabulary, staging
phrases, chat residue, placeholders, and flat sentence rhythm. A hit is a thing
to look at, not a verdict. Fenced code, inline code and Markdown blockquotes are
skipped so quoted sources and code are not flagged.

Usage:
  check.py --file draft.md
  check.py --text "Some text"
  some-command | check.py --stdin
Exit code 1 if any em dash, chat residue or placeholder is found, else 0.
"""
import argparse
import re
import statistics
import sys

VOCAB = [
    "delve", "bolster", "illuminate", "elucidate", "transcend", "intertwine", "underscor",
    "tapestry", "synergy", "beacon", "interplay", "intricate", "intricacies", "myriad",
    "plethora", "vibrant", "meticulous", "multifaceted", "pivotal", "paramount", "testament",
    "realm", "landscape", "garner", "showcas", "foster", "leverag", "utiliz", "facilitat",
    "nestled", "enduring", "commendable", "unparalleled", "ever-evolving", "cutting-edge",
    "game-changing", "groundbreaking", "holistic",
    "shed light on", "grapple with", "navigate the complexities", "paving the way",
    "rich history", "plays a vital role", "deep dive", "at its core", "in today's world",
    "look no further",
]
MARKETING = [
    "empower", "unlock", "elevate", "seamless", "streamline", "revolutioniz", "supercharg",
    "harness the power", "next-level", "best-in-class", "world-class", "one-stop", "effortless",
    "explore", "discover", "journey", "marketplace", "exchange",
]
TRANSITIONS = [
    "Additionally", "Furthermore", "Moreover", "Importantly", "Crucially", "Critically",
    "Notably", "Ultimately", "In conclusion", "Lastly", "That said", "Needless to say",
    "In other words", "At the end of the day", "As we can see",
]
STAGING = [
    r"here'?s (the thing|what|why)", r"let me (be clear|explain)", r"let'?s (break it down|dive)",
    r"what (nobody|no one) tells you", r"what most people (miss|get wrong)", r"the (real story|truth) is",
    r"the best part", r"the bottom line", r"key takeaway", r"that'?s the real", r"let that sink in",
    r"\bthoughts\?", r"what do you think\?", r"\bnot (just|only) [^.,;]{1,40}\bbut\b",
    r"\bit'?s not [^.]{1,40}, it'?s\b", r"\bisn'?t about [^.]{1,40}\. it'?s about\b",
    r"\bserves as\b", r"\bstands as\b", r"\bboasts\b", r"\ba testament to\b",
    r", (highlighting|underscoring|reflecting|showcasing|emphasizing|demonstrating) ",
    r"\b(this|that) means that\b", r"\bas you can see\b", r"\bnotice how\b",
]
HEDGES = [
    "arguably", "it's worth noting", "it is worth noting", "it's important to note",
    "it is important to note", "could potentially", "may potentially", "to some extent",
    "in many ways",
]
OPED = ["posits", "contends", "asserts", "maintains that"]
CHAT = [
    r"great question", r"\bcertainly!", r"\bof course!", r"\babsolutely!", r"i hope this helps",
    r"i hope this (email )?finds you", r"let me know if you'?d like", r"feel free to reach out",
    r"here'?s (a|the) revised", r"here is the revised", r"\bas an ai\b", r"as of my last update",
    r"happy to help",
]
PLACEHOLDER = [
    r"\[(?i:your|insert)[^\]]*\]", r"\bTODO\b", r"\bTBD\b", r"\b\d{4}-XX-XX\b", r"INSERT_",
    r"lorem ipsum", r"utm_source=(chatgpt|openai)", r"contentReference\[oaicite",
]
HOUSE = [r"\b(free )?colou?red (people|persons?|men|women|children|population|families|residents|voters|schools?|troops|soldiers|households?)\b", r"\bfree colou?red\b"]


def strip_code(text):
    """Blank out fenced code, inline code and blockquote lines, keeping line numbers."""
    out, fence = [], None  # (char, length) of the open fence
    for line in text.split("\n"):
        m = re.match(r"\s{0,3}(`{3,}|~{3,})(.*)$", line)
        if m and fence is None:
            fence = (m.group(1)[0], len(m.group(1)))
            out.append("")
            continue
        if m and fence and m.group(1)[0] == fence[0] and len(m.group(1)) >= fence[1] and not m.group(2).strip():
            fence = None
            out.append("")
            continue
        if fence or line.lstrip().startswith(">"):
            out.append("")
            continue
        out.append(re.sub(r"(`+)(?!`).*?(?<!`)\1(?!`)", "``", line))
    return out


def word_re(term):
    term = re.escape(term).replace(r"\ ", r"\s+")
    return re.compile(r"\b" + term, re.I)


def sentences(lines):
    text = "\n".join(l for l in lines if l.strip() and not l.lstrip().startswith(("#", "|", "-", "*")) and not re.match(r"\s*\d+\.\s", l))
    parts = re.split(r"(?<=[.!?])\s+|\n\s*\n", text)
    return [p.strip() for p in parts if len(p.split()) >= 3]


def check(text):
    lines = strip_code(text)
    hits = []  # (severity, label, line_no, snippet)

    def scan(label, patterns, sev, compiled=False, case=False):
        for pat in patterns:
            rx = re.compile(pat, 0 if case else re.I) if compiled else word_re(pat)
            for n, line in enumerate(lines, 1):
                for m in rx.finditer(line):
                    s = max(0, m.start() - 30)
                    hits.append((sev, label, n, line[s:m.end() + 30].strip()))

    scan("em dash", ["—"], "fix", compiled=True)
    scan("spaced en dash", [r"\s–\s"], "fix", compiled=True)
    scan("semicolon", [r";"], "look", compiled=True)
    scan("chat residue", CHAT, "fix", compiled=True)
    scan("placeholder", PLACEHOLDER, "fix", compiled=True, case=True)
    scan("staging", STAGING, "look", compiled=True)
    scan("stock vocabulary", VOCAB, "look")
    scan("marketing register", MARKETING, "look")
    scan("filler hedge", HEDGES, "look")
    scan("op-ed verb", OPED, "look")
    scan("house term", HOUSE, "look", compiled=True)
    tr = [re.compile(r"(^|[.!?]\s+)" + re.escape(t) + r"\b") for t in TRANSITIONS]
    for n, line in enumerate(lines, 1):
        for rx in tr:
            for m in rx.finditer(line.lstrip("-* ")):
                hits.append(("look", "filler transition", n, line.strip()[:70]))
        if re.search(r"\*\*[^*]+\*\*", line) and len(re.findall(r"\*\*[^*]+\*\*", line)) >= 3:
            hits.append(("look", "bold sprinkled", n, line.strip()[:70]))
        if re.match(r"\s*#{1,6}\s", line):
            words = re.findall(r"[A-Za-z][A-Za-z']+", line)
            caps = [w for w in words[1:] if w[0].isupper() and w.lower() not in ("i",)]
            if len(words) >= 4 and len(caps) >= len(words) - 2:
                hits.append(("look", "title case heading", n, line.strip()))

    notes = []
    sents = sentences(lines)
    if len(sents) >= 8:
        lens = [len(s.split()) for s in sents]
        med = statistics.median(lens)
        band = sum(abs(x - med) <= 5 for x in lens) / len(lens)
        if band > 0.7:
            notes.append(f"flat rhythm: {band:.0%} of {len(lens)} sentences within 5 words of the median ({med:g})")
        starts = [s.split()[0].lower().strip("\"'*") for s in sents]
        common = sum(w in ("the", "it", "it's", "this", "that's", "that", "you") for w in starts) / len(starts)
        if common > 0.4:
            notes.append(f"openers: {common:.0%} of sentences start with The/It/This/That/You")
    return hits, notes


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    g = ap.add_mutually_exclusive_group(required=True)
    g.add_argument("--file", help="path to the draft")
    g.add_argument("--text", help="the draft as a string")
    g.add_argument("--stdin", action="store_true", help="read the draft from stdin")
    a = ap.parse_args()
    text = open(a.file, encoding="utf-8").read() if a.file else a.text if a.text is not None else sys.stdin.read()
    hits, notes = check(text)
    hits = sorted(set(hits), key=lambda h: (h[0] != "fix", h[2]))
    for sev, label, n, snip in hits:
        print(f"{sev:4}  line {n:<4} {label}: {snip}")
    for note in notes:
        print(f"look  {note}")
    fixes = sum(h[0] == "fix" for h in hits)
    print(f"\n{fixes} to fix, {len(hits) - fixes + len(notes)} to look at")
    sys.exit(1 if fixes else 0)


if __name__ == "__main__":
    main()
