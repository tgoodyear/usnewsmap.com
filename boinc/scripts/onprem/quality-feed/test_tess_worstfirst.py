import unittest

import tess_worstfirst as t


class SelectSql(unittest.TestCase):
    def test_only_english(self):
        for lang in ("deu", "frak2021", "srp"):
            with self.assertRaises(SystemExit):
                t.select_sql(lang, 100, 10, None)

    def test_cut_is_one_of(self):
        with self.assertRaises(SystemExit):
            t.select_sql("eng", 100, None, None)
        with self.assertRaises(SystemExit):
            t.select_sql("eng", 100, 5, 3.0)

    def test_filters_present(self):
        sql = t.select_sql("eng", 100, 20000, None)
        for needle in ("language_source = 'page'", "boinc_page_provenance", "boinc_page_holds",
                       "ocr_outputs", "LIMIT 20000", "NOT invalid"):
            self.assertIn(needle, sql)
        self.assertIn("lm_oov >= 5.5", t.select_sql("eng", 100, None, 5.5))

    def test_chunks(self):
        self.assertEqual([len(c) for c in t.chunk(list("abcdefg"), 3)], [3, 3, 1])


if __name__ == "__main__":
    unittest.main()


class Decide(unittest.TestCase):
    import datetime as _d
    cfg = dict(t.DEFAULTS, rebuild_done_marker="/nonexistent/marker")
    now = _d.datetime(2026, 11, 16, tzinfo=_d.timezone.utc)

    def st(self, **kw):
        base = dict(wave=1, wave_queued=20000, wave_skipped=0, outstanding=20000, exhausted=False,
                    wave_started="2026-11-15T12:00:00Z", campaign="c")
        return dict(base, **kw)

    def test_tops_up(self):
        self.assertEqual(t.decide(self.cfg, self.st(wave_queued=3000), self.now, 0, None)[0], "enqueue")

    def test_waits_for_gain(self):
        self.assertEqual(t.decide(self.cfg, self.st(), self.now, 19500, None)[0], "idle")

    def test_stops_on_low_pass_rate_or_gain(self):
        for g in ({"pass_rate": 0.79, "median_gain": 1.0}, {"pass_rate": 0.9, "median_gain": 0.0}):
            self.assertEqual(t.decide(self.cfg, self.st(), self.now, 19500, g)[0], "stop")

    def test_next_wave(self):
        g = {"pass_rate": 0.9, "median_gain": 0.5}
        self.assertEqual(t.decide(self.cfg, self.st(), self.now, 19500, g)[0], "next_wave")
        self.assertEqual(t.wave_size(self.cfg, 2), 50000)
        self.assertEqual(t.wave_size(self.cfg, 9), 100000)

    def test_paused_after_cutoff_until_marker(self):
        late = self._d.datetime(2026, 11, 18, 14, 0, tzinfo=self._d.timezone.utc)
        self.assertEqual(t.decide(self.cfg, self.st(wave_queued=0), late, 0, None)[0], "idle")

    def test_exhausted(self):
        self.assertEqual(t.decide(self.cfg, self.st(wave_queued=0, exhausted=True), self.now, 0, None)[0], "stop")

    def test_exhausted_mid_wave_stops(self):
        self.assertEqual(t.decide(self.cfg, self.st(wave_queued=5000, exhausted=True), self.now, 0, None)[0], "stop")

    def test_timeout_beats_queue_full(self):
        old = self.st(wave_queued=10000, wave_skipped=0, outstanding=70000, wave_started="2026-10-01T00:00:00Z")
        cfg = dict(self.cfg, wave_sizes=[100000])
        self.assertEqual(t.decide(cfg, old, self.now, 100, None)[0], "idle")  # done, waiting for gain
        g = {"pass_rate": 0.1, "median_gain": 1}
        self.assertEqual(t.decide(cfg, old, self.now, 100, g)[0], "stop")
