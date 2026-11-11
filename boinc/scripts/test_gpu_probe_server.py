"""The probe server's built-in templates stay in step with boinc/apps (#1435)."""
import importlib.util
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent


def load():
    spec = importlib.util.spec_from_file_location("gpu_probe_server", HERE / "gpu_probe_server.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class TemplateTest(unittest.TestCase):
    def test_qwen_output_template_matches_the_app_template(self):
        # ensure_qwen_campaigns writes it over templates/usnm_qwen_out, so a stale copy would
        # bring back an older upload limit.
        self.assertEqual(load().TEMPLATE_QWEN_OUT, (HERE.parent / "apps" / "usnm_qwen_out").read_text())


if __name__ == "__main__":
    unittest.main()
