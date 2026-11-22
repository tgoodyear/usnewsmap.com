"""Import paths for the render-vs-scan test (#2103): the survey scripts, the damage metric and the look-alike tool.

Importing this module puts `scripts/ocr-survey` (gold.py, altotext.py), `ocr/ja-ocr` (damage_v2.py) and
`edge/check/tools` (lookalike.py) on sys.path, so the scripts here reuse them instead of copying them.
"""

import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
SURVEY = ROOT / "scripts" / "ocr-survey"
DAMAGE_DIR = ROOT / "ocr" / "ja-ocr"
CHECK_TOOLS = ROOT / "edge" / "check" / "tools"
DAMAGE_WORDS = ROOT / "crates" / "usnm-core" / "data" / "damage-words"

for p in (HERE, SURVEY, DAMAGE_DIR, CHECK_TOOLS):
    if str(p) not in sys.path:
        sys.path.insert(0, str(p))
