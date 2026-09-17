#!/usr/bin/env python3
"""Import reviewed tracked files from the pinned Hen commit, never the worktree."""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import subprocess

COMMIT = "49042697f11bc86dfa8e6bd10ef0dc07cffbaf84"
REPOSITORY = "https://github.com/Hen-Local/Hen-Local-Translator.git"
ROOT_FILES = {"Cargo.toml", "Cargo.lock", "LICENSE"}
DIRECTORIES = (
    "hen-local-translator-shell/", "moxin-dora-bridge/", "hen-local-init/",
    "node-hub/dora-qwen3-asr/", "node-hub/dora-qwen35-translator/",
)
SCRIPTS = {
    "scripts/run_translator_desktop.sh", "scripts/macos_preflight.sh",
    "scripts/macos_bootstrap.sh", "scripts/init_qwen3_models.sh",
    "scripts/build_macos_app.sh", "scripts/build_macos_dmg.sh",
    "scripts/macos_sign_and_notarize.sh",
}
EXCLUDED = {
    "hen-local-translator-shell/src/account.rs",
    "hen-local-translator-shell/account_config.example.json",
    "hen-local-translator-shell/license-public-key.b64",
    "hen-local-translator-shell/ui/src/VoiceLab.svelte",
    "hen-local-translator-shell/ui/src/voice-lab.ts",
    "hen-local-translator-shell/ui/src/voice-lab.css",
    "hen-local-translator-shell/ui/voice-lab.html",
    "node-hub/dora-qwen35-translator/src/bin/translator-prompt-harness.rs",
}


def git(source: Path, *args: str) -> bytes:
    return subprocess.check_output(["git", "-C", str(source), *args])


def selection(source: Path) -> list[dict]:
    result = []
    for entry in git(source, "ls-tree", "-rz", COMMIT).split(b"\0"):
        if not entry:
            continue
        meta, raw_path = entry.split(b"\t", 1)
        mode, kind, blob = meta.decode().split()
        path = raw_path.decode()
        if path in EXCLUDED or not (
            path in ROOT_FILES or path in SCRIPTS or path.startswith(DIRECTORIES)
        ):
            continue
        if kind != "blob" or mode not in {"100644", "100755"}:
            raise ValueError(f"Unsupported imported entry: {path} ({mode})")
        relative = PurePosixPath(path)
        if relative.is_absolute() or ".." in relative.parts:
            raise ValueError(f"Unsafe source path: {path}")
        destination = path.replace("hen-local-translator-shell/", "forum-shell/", 1)
        content = git(source, "cat-file", "blob", blob)
        result.append({
            "source_path": path, "destination_path": f"desktop/{destination}",
            "source_blob": blob, "source_sha256": hashlib.sha256(content).hexdigest(),
            "source_mode": mode, "size": len(content),
        })
    return result


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", required=True, type=Path)
    parser.add_argument("--apply", action="store_true", help="Import into a new desktop directory")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    files = selection(args.source)
    manifest = {
        "schema_version": 1, "repository": REPOSITORY, "commit": COMMIT,
        "license": "Apache-2.0", "import_method": "git-object-allowlist",
        "note": "Hashes describe the upstream baseline before Forum adaptations, not current files.",
        "excluded_paths": sorted(EXCLUDED), "files": files,
    }
    if args.apply:
        desktop = root / "desktop"
        manifest_path = root / "docs/engineering/hen-import-manifest.json"
        if desktop.exists() or manifest_path.exists():
            raise SystemExit("Refusing to overwrite an existing import or source manifest.")
        # Validate and fetch all source content before creating the destination.
        contents = {item["source_blob"]: git(args.source, "cat-file", "blob", item["source_blob"])
                    for item in files}
        for item in files:
            destination = root / item["destination_path"]
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(contents[item["source_blob"]])
            destination.chmod(int(item["source_mode"], 8) & 0o777)
        manifest_path.write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps({"commit": COMMIT, "files": len(files),
                      "bytes": sum(item["size"] for item in files), "applied": args.apply}))


if __name__ == "__main__":
    main()
