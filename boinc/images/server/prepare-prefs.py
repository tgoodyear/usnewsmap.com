"""Keep stock BOINC preferences intact while extending its authenticated form."""
from pathlib import Path
import re


def extend_stock(source: str) -> str:
    for suffix in ("default", "parse", "edit", "parse_form", "show"):
        source, count = re.subn(
            rf"\bfunction project_specific_prefs_{suffix}\(",
            f"function usnm_stock_prefs_{suffix}(",
            source,
        )
        if count != 1:
            raise ValueError(f"unexpected stock BOINC preference function: {suffix}")
    return source


if __name__ == "__main__":
    source = Path("/usr/local/boinc/html/project.sample/project_specific_prefs.inc")
    destination = Path("/usr/local/lib/usnm-boinc/html/project/usnm_stock_prefs.inc")
    destination.write_text(extend_stock(source.read_text()))
