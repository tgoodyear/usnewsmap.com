import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import render_samples as rs  # noqa: E402


class Pick(unittest.TestCase):
    def test_round_robin_over_pages(self):
        tasks = [{"id": f"en-read/t/p{p}~{k}", "page": f"p{p}"} for p in range(3) for k in range(5)]
        got = rs.pick(tasks, 6, "2122")
        self.assertEqual(len(got), 6)
        self.assertEqual(sorted(t["page"] for t in got), ["p0", "p0", "p1", "p1", "p2", "p2"])
        self.assertEqual(got, rs.pick(list(reversed(tasks)), 6, "2122"))

    def test_fewer_tasks_than_asked(self):
        self.assertEqual(len(rs.pick([{"id": "a", "page": "p"}], 12, "1")), 1)


if __name__ == "__main__":
    unittest.main()
