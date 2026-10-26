import json
import unittest

from coverage import audit


class CoverageTests(unittest.TestCase):
    def manifest(self):
        sample = dict(id='one', source='test', source_page_id='page', languages=['fra'],
                      damage={'status': 'unannotated'}, unit='crop',
                      reference_status='human-verified', mix='monolingual')
        return dict(format='usnm-ocr-bench', release='v1.1.0', samples=[sample],
                    subsets={'a': {'members': ['one']}, 'b': {'members': ['one']}})

    def test_unknown_is_not_clean_and_overlapping_subsets_are_not_counted_twice(self):
        result = audit(json.dumps(self.manifest()).encode())
        self.assertEqual(result['samples'], 1)
        self.assertEqual(result['matrix'][0]['severity'], 'unknown')
        self.assertEqual(result['damage_status_counts'], {'unannotated': 1})
        self.assertNotIn('fra', result['absent_target_languages'])
        self.assertIn('jpn', result['absent_target_languages'])

    def test_distinct_damage_units_and_shared_pages(self):
        m = self.manifest()
        other = dict(m['samples'][0], id='two', unit='page',
                     damage=dict(status='reviewed', labels=['blur'], severity='mild', evidence='fixture'))
        m['samples'].append(other)
        result = audit(json.dumps(m).encode())
        self.assertEqual((result['samples'], result['pages'], len(result['matrix'])), (2, 1, 2))

    def test_duplicate_ids_and_unknown_status_are_errors(self):
        m = self.manifest()
        m['samples'].append(m['samples'][0])
        with self.assertRaises(ValueError):
            audit(json.dumps(m).encode())
        m = self.manifest()
        m['samples'][0]['damage']['status'] = 'clean'
        with self.assertRaises(ValueError):
            audit(json.dumps(m).encode())

    def test_unannotated_damage_rejects_severity_and_preserves_narrative_observations(self):
        for damage in [
            dict(status='unannotated', labels=[], severity='none', evidence=''),
            dict(status='unannotated', observations='not an array'),
            dict(status='unannotated', observations=[1]),
        ]:
            m = self.manifest()
            m['samples'][0]['damage'] = damage
            with self.assertRaises(ValueError):
                audit(json.dumps(m).encode())
        m = self.manifest()
        m['samples'][0]['damage']['observations'] = ['Inherited pilot narrative, not reviewed severity']
        result = audit(json.dumps(m).encode())
        self.assertEqual(result['matrix'][0]['severity'], 'unknown')
        self.assertEqual(result['matrix'][0]['damage_labels'], [])

    def test_reviewed_damage_requires_valid_labels_severity_and_evidence(self):
        for damage in [
            dict(status='reviewed', labels=[], severity='mild', evidence='fixture'),
            dict(status='reviewed', labels=['invented'], severity='mild', evidence='fixture'),
            dict(status='reviewed', labels=['blur', 'blur'], severity='mild', evidence='fixture'),
            dict(status='reviewed', labels=[], severity='none', evidence=''),
            dict(status='reviewed', labels=[], severity='none'),
        ]:
            m = self.manifest()
            m['samples'][0]['damage'] = damage
            with self.assertRaises(ValueError):
                audit(json.dumps(m).encode())
