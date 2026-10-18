#!/usr/bin/env python3
"""Count actual manifest samples, never summed overlapping subset memberships."""
import argparse
from collections import Counter, defaultdict
import hashlib
import json
from pathlib import Path

TARGET_LANGUAGES = ('eng', 'deu', 'jpn', 'spa', 'fra', 'ita', 'dan', 'fin', 'lit',
                    'ces', 'rus', 'srp', 'heb', 'yid')


def audit(data: bytes) -> dict:
    manifest = json.loads(data)
    if manifest.get('format') != 'usnm-ocr-bench':
        raise ValueError('not a benchmark manifest')
    groups = defaultdict(list)
    ids, languages, pages = set(), set(), set()
    damage_counts, mix_counts = Counter(), Counter()
    for sample in manifest['samples']:
        if sample['id'] in ids:
            raise ValueError(f"duplicate sample {sample['id']}")
        ids.add(sample['id'])
        langs = tuple(sorted(set(sample['languages'])))
        languages.update(langs)
        damage = sample['damage']
        status = damage['status']
        if status not in ('unannotated', 'reviewed'):
            raise ValueError(f'unknown damage status {status}')
        labels = tuple(sorted(damage.get('labels', [])))
        severity = damage.get('severity', 'unknown')
        key = (langs, status, labels, severity, sample['unit'], sample['reference_status'])
        page = (sample['source'], sample['source_page_id'])
        pages.add(page)
        groups[key].append(page)
        damage_counts[status] += 1
        mix_counts[sample['mix']] += 1
    matrix = []
    for (langs, status, labels, severity, unit, ref_status), group in sorted(groups.items()):
        matrix.append(dict(languages=list(langs), damage_status=status,
                           damage_labels=list(labels), severity=severity, unit=unit,
                           reference_status=ref_status, samples=len(group), pages=len(set(group))))
    return dict(release=manifest['release'], manifest_sha256=hashlib.sha256(data).hexdigest(),
                samples=len(ids), pages=len(pages), matrix=matrix,
                damage_status_counts=dict(sorted(damage_counts.items())),
                mix_counts=dict(sorted(mix_counts.items())),
                absent_target_languages=sorted(set(TARGET_LANGUAGES) - languages),
                note='Unique manifest samples; pages overlap across matrix rows. Unknown damage is not clean. '
                     'Language presence does not imply mixed-language text or a completed per-track tier.')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    result = audit(args.manifest.read_bytes())
    with args.out.open('x', encoding='utf-8') as stream:
        stream.write(json.dumps(result, ensure_ascii=False, indent=2, sort_keys=True) + '\n')


if __name__ == '__main__':
    main()
