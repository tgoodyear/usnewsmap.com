#!/usr/bin/env python3
"""Render only local hierarchy settings; never fetch secrets or replace the share link."""
import argparse
import os
from pathlib import Path
import xml.etree.ElementTree as ET


def render(project: Path, config: Path, template: Path) -> None:
    project = project.resolve(strict=True)
    if not config.is_absolute():
        raise ValueError("local config path must be absolute")
    link = project / "config.xml"
    if not link.is_symlink() or os.readlink(link) != str(config):
        raise ValueError("unexpected shared configuration link; refusing to replace it")
    if config.exists() or config.is_symlink():
        raise ValueError("local configuration already exists; refusing to overwrite it")
    source = ET.parse(template)
    fanout = source.findtext("./config/uldl_dir_fanout")
    if fanout is None or not fanout.isdigit() or not 1 <= int(fanout) <= 65536:
        raise ValueError("invalid hierarchy fanout")
    root = ET.Element("boinc")
    settings = ET.SubElement(root, "config")
    ET.SubElement(settings, "download_dir").text = str(project / "download") + "/"
    ET.SubElement(settings, "uldl_dir_fanout").text = fanout
    config.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    with os.fdopen(os.open(config, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "wb") as out:
        out.write(ET.tostring(root, encoding="utf-8", xml_declaration=True))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--project", type=Path, required=True)
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--template", type=Path, required=True)
    args = parser.parse_args()
    render(args.project, args.config, args.template)
