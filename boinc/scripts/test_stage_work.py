"""Compile the pinned scheduler decision, not a Python approximation.

The base-image build supplies BOINC_SOURCE_ROOT before applying the staging patch.
Local runs may use a checkout of the exact Containerfile-pinned BOINC commit.
"""
import importlib.util
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location(
    "stage_work", Path(__file__).with_name("patch-server-stage-work.py"))
STAGE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(STAGE)


@unittest.skipUnless(os.environ.get("BOINC_SOURCE_ROOT"), "pinned BOINC source required (base-image build)")
class PinnedStaging(unittest.TestCase):
    def setUp(self):
        self.root = Path(os.environ["BOINC_SOURCE_ROOT"])

    def test_patch_applies_once_and_exposes_only_single_unassigned_creation(self):
        original = (self.root / "tools/create_work.cpp").read_text()
        patched = STAGE.patch_source(original)
        self.assertIn('jd.no_new_results = true;', patched)
        self.assertIn('use_stdin || jd.assign_flag || jd.wu.id', patched)
        self.assertIn('wu.transitioner_flags = TRANSITION_NO_NEW_RESULTS;', patched)
        with self.assertRaises(ValueError):
            STAGE.patch_source(patched)
        backend = (self.root / "tools/backend_lib.cpp").read_text()
        self.assertIn('if (wu.transitioner_flags) {\n        wu.transition_time = INT_MAX;', backend)
        transitioner = (self.root / "sched/transitioner.cpp").read_text()
        self.assertIn('wu_item.transitioner_flags != TRANSITION_NO_NEW_RESULTS\n'
                      '            && n_new_results_needed > 0', transitioner)

    def test_actual_need_targeted_instance_allows_two_hosts_but_not_generic_replicas(self):
        source = (self.root / "sched/sched_assign.cpp").read_text()
        start = source.index('bool need_targeted_instance(')
        function = source[start:source.index('\n}', start) + 2]
        types = (self.root / "db/boinc_db_types.h").read_text()
        names = ('RESULT_SERVER_STATE_INACTIVE', 'RESULT_SERVER_STATE_UNSENT',
                 'RESULT_SERVER_STATE_IN_PROGRESS', 'RESULT_SERVER_STATE_OVER',
                 'RESULT_OUTCOME_SUCCESS', 'VALIDATE_STATE_INVALID', 'TRANSITION_NO_NEW_RESULTS')
        definitions = '\n'.join(re.search(r'^#define ' + name + r'\s+\d+', types, re.M)[0] for name in names)
        harness = r'''
#include <cassert>
#include <cstdio>
#include <ctime>
#include <climits>
#include <vector>
struct WORKUNIT {
    unsigned long id=300;
    int error_mask=0, need_validate=0, canonical_resultid=0;
    int transition_time=INT_MAX, target_nresults=2;
};
struct Row { int hostid, server_state, outcome=0, validate_state=0; };
static std::vector<Row> rows;
struct DB_RESULT : Row {
    size_t index=0;
    int enumerate(const char *) {
        if (index == rows.size()) return 1;
        static_cast<Row&>(*this)=rows[index++];
        return 0;
    }
};
'''
        checks = r'''
int main() {
    static_assert(TRANSITION_NO_NEW_RESULTS == 2);
    WORKUNIT wu;
    assert(need_targeted_instance(wu, 2));
    rows.push_back({2, RESULT_SERVER_STATE_IN_PROGRESS});
    assert(!need_targeted_instance(wu, 2)); // never send twice to one host
    assert(need_targeted_instance(wu, 3));
    rows.push_back({3, RESULT_SERVER_STATE_IN_PROGRESS});
    assert(!need_targeted_instance(wu, 4));
    rows={{2, RESULT_SERVER_STATE_OVER, RESULT_OUTCOME_SUCCESS},
          {3, RESULT_SERVER_STATE_OVER, RESULT_OUTCOME_SUCCESS}};
    assert(!need_targeted_instance(wu, 4));
    rows={{0, RESULT_SERVER_STATE_UNSENT}, {0, RESULT_SERVER_STATE_UNSENT}};
    assert(!need_targeted_instance(wu, 2)); // reproduces the reviewed stall
    assert(!need_targeted_instance(wu, 3));
    rows.clear();
    wu.error_mask=1; assert(!need_targeted_instance(wu, 2)); wu.error_mask=0;
    wu.need_validate=1; assert(!need_targeted_instance(wu, 2)); wu.need_validate=0;
    wu.canonical_resultid=1; assert(!need_targeted_instance(wu, 2)); wu.canonical_resultid=0;
    wu.transition_time=1; assert(!need_targeted_instance(wu, 2));
}
'''
        # All scratch stays under the caller's chosen project/build working directory.
        with tempfile.TemporaryDirectory(prefix=".stage-work-", dir=Path.cwd()) as folder:
            path = Path(folder)
            (path / "check.cpp").write_text(definitions + harness + function + checks)
            subprocess.run(["c++", "-std=c++17", "-Wall", "-Wextra", "-Werror",
                            *(["-Wno-deprecated-declarations"] if sys.platform == "darwin" else []),
                            str(path / "check.cpp"), "-o", str(path / "check")], check=True)
            subprocess.run([str(path / "check")], check=True)


if __name__ == '__main__':
    unittest.main()
