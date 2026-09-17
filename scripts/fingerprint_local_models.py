#!/usr/bin/env python3
"""Fingerprint the files used by a local model probe, without downloading weights."""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path


def fingerprint(directory: Path) -> dict:
    if not directory.is_dir():
        raise ValueError(f"Missing model directory: {directory}")
    files = []
    for path in sorted(directory.iterdir()):
        if not path.is_file() or path.name.startswith("."):
            continue
        if path.suffix not in {".safetensors", ".json", ".txt", ".jinja"}:
            continue
        with path.open("rb") as stream:
            digest = hashlib.file_digest(stream, "sha256").hexdigest()
        files.append({"name": path.name, "bytes": path.stat().st_size, "sha256": digest})
    if not any(item["name"].endswith(".safetensors") for item in files):
        raise ValueError(f"No safetensors weights found in {directory}")
    return {
        "directory_name": directory.name,
        "remote_revision": None,
        "files": files,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directories", nargs="+", type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    report = {
        "schema_version": 1,
        "purpose": "F01 local probe reproducibility; not a release download manifest",
        "models": [fingerprint(path) for path in args.directories],
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    print(f"Fingerprinted {len(report['models'])} local models: {args.output}")


if __name__ == "__main__":
    main()
