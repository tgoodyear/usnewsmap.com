#!/usr/bin/env python3
"""Check local Markdown destinations/anchors under docs/, without network requests."""

import argparse
import html
import re
import sys
from pathlib import Path
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parents[2]


def prose(text):
    """Keep line numbers while excluding fenced examples."""
    fence = None
    lines = []
    for line in text.splitlines():
        marker = re.match(r"^ {0,3}(`{3,}|~{3,})", line)
        if fence:
            if marker and marker[1][0] == fence[0] and len(marker[1]) >= len(fence):
                fence = None
            lines.append("")
        elif marker:
            fence = marker[1]
            lines.append("")
        else:
            lines.append(line)
    return "\n".join(lines)


def anchors(text):
    clean = prose(text)
    found = set(re.findall(r'<a\s+(?:[^>]*?\s)?(?:id|name)=["\']([^"\']+)["\']', clean))
    counts = {}
    for match in re.finditer(r"^ {0,3}#{1,6}\s+(.+?)\s*#*\s*$", clean, re.M):
        heading = re.sub(r"\[([^\]]+)\]\([^)]*\)", r"\1", match[1])
        heading = html.unescape(re.sub(r"<[^>]+>", "", heading)).lower()
        slug = re.sub(r"[^\w -]", "", heading).replace(" ", "-")
        number = counts.get(slug, 0)
        counts[slug] = number + 1
        found.add(f"{slug}-{number}" if number else slug)
    return found


def links(text):
    clean = prose(text)
    clean = re.sub(r"(`+)(.*?)\1", lambda m: " " * len(m[0]), clean)
    pattern = r"!?\[[^\]\n]*\]\(\s*|^ {0,3}\[[^\]\n]+\]:[ \t]*"
    for match in re.finditer(pattern, clean, re.M):
        rest = clean[match.end():].split("\n", 1)[0]
        if rest.startswith("<"):
            end = rest.find(">")
            if end == -1:
                continue
            target = rest[1:end]
        else:
            depth = 0
            escaped = False
            end = 0
            for end, char in enumerate(rest):
                if escaped:
                    escaped = False
                elif char == "\\":
                    escaped = True
                elif char == "(":
                    depth += 1
                elif char == ")":
                    if depth == 0:
                        break
                    depth -= 1
                elif char.isspace():
                    break
            else:
                end = len(rest)
            target = re.sub(r"\\([\\()<>])", r"\1", rest[:end])
        if target:
            yield clean.count("\n", 0, match.start()) + 1, target


def check(paths):
    failures = []
    for path in paths:
        for line, target in links(path.read_text()):
            url = urlsplit(target)
            if url.scheme or url.netloc:
                continue
            dest = path.parent / unquote(url.path) if url.path else path
            if not dest.exists():
                failures.append((path, line, f"missing file: {target}"))
            elif url.fragment and dest.suffix == ".md":
                if unquote(url.fragment) not in anchors(dest.read_text()):
                    failures.append((path, line, f"missing anchor: {target}"))
    return failures


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--paths", nargs="+", type=Path, help="Markdown files (default: docs/**/*.md)")
    args = parser.parse_args()
    paths = args.paths or sorted(ROOT.joinpath("docs").rglob("*.md"))
    failures = check(paths)
    for path, line, why in failures:
        print(f"{path}:{line}: {why}", file=sys.stderr)
    print(f"{len(paths)} Markdown files; {len(failures)} broken local links")
    return bool(failures)


if __name__ == "__main__":
    sys.exit(main())
