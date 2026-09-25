#!/usr/bin/env python3
"""Build the macOS arm64 sidecar from hash-pinned upstream artifacts.

The bootstrap interpreter only needs Python 3.12+ stdlib. The final runtime is
the pinned standalone CPython archive, never the bootstrap interpreter/venv.
This script writes a NEW output directory and never replaces an existing one.
"""

from __future__ import annotations

import argparse
import csv
import base64
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.parse
import urllib.request

REPO = Path(__file__).resolve().parents[2]
WORKER = REPO / "services/meeting-worker"
LOCK = WORKER / "packaging/runtime-macos-arm64.lock.json"
OWNER = "ai-vision-forum-meeting-worker-v1"
ANALYSIS_TASK_TYPES = ["insight", "minutes", "event_report", "suggested_questions",
                       "redaction_review", "closing_brief"]
PROMPT_DIRECTORY = Path("python/lib/python3.12/site-packages/forum_meeting_worker/prompts/v1")


def analysis_prompt_manifest(staged: Path, source_hashes: dict[str, str]) -> dict:
    """Verify the wheel installed the same six complete prompts we packaged."""
    directory = staged / PROMPT_DIRECTORY
    expected = {f"{kind}.txt" for kind in ANALYSIS_TASK_TYPES}
    if directory.is_symlink() or not directory.is_dir() or {p.name for p in directory.iterdir()} != expected:
        raise ValueError("Worker wheel must contain exactly six v1 analysis prompt resources.")
    result = {}
    for kind in ANALYSIS_TASK_TYPES:
        path = directory / f"{kind}.txt"
        if path.is_symlink() or not path.is_file() or not 0 < path.stat().st_size <= 64 * 1024:
            raise ValueError("Analysis prompts must be bounded regular files.")
        path.read_text(encoding="utf-8")
        actual_hash = sha256(path)
        source_key = f"src/forum_meeting_worker/prompts/v1/{kind}.txt"
        if source_hashes.get(source_key) != actual_hash:
            raise ValueError("Installed analysis prompt differs from the packaged source.")
        result[kind] = {"prompt_version": f"{kind}-v1", "relative_path": str(PROMPT_DIRECTORY / path.name),
                        "sha256": actual_hash, "size_bytes": path.stat().st_size}
    return result


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def new_output(path: Path) -> Path:
    if not path.is_absolute() or ".." in path.parts:
        raise ValueError("--output must be absolute, without '..'.")
    if path.exists() or path.is_symlink():
        raise ValueError("Output already exists; select a new directory. No overwrite or deletion is supported.")
    parent = path.parent.resolve(strict=True)
    if not parent.is_dir():
        raise ValueError("Output parent must be an existing directory.")
    return parent / path.name


def validate_artifact(artifact: dict) -> None:
    name = artifact["filename"]
    if not isinstance(name, str) or Path(name).name != name or name in {"", ".", ".."}:
        raise ValueError("Unsafe artifact filename.")
    url = urllib.parse.urlsplit(artifact["url"])
    if url.scheme != "https" or url.hostname not in {"github.com", "files.pythonhosted.org"}:
        raise ValueError("Artifacts must use pinned official HTTPS sources.")
    if not re.fullmatch(r"[a-f0-9]{64}", artifact["sha256"]):
        raise ValueError("Artifact must have a SHA-256 pin.")
    if type(artifact["size"]) is not int or artifact["size"] <= 0:
        raise ValueError("Artifact must have a positive pinned size.")


def fetch(artifact: dict, cache: Path, offline: bool) -> Path:
    validate_artifact(artifact)
    target = cache / artifact["filename"]
    if target.is_symlink():
        raise ValueError("Artifact cache files must not be symlinks.")
    if target.exists():
        if target.stat().st_size != artifact["size"] or sha256(target) != artifact["sha256"]:
            raise ValueError(f"Cached artifact hash/size mismatch: {target.name}; refusing to replace it.")
        return target
    if offline:
        raise ValueError(f"Missing offline artifact: {target.name}")
    print(f"Fetching pinned artifact {target.name}", file=sys.stderr)
    with tempfile.NamedTemporaryFile(dir=cache, prefix=".download-", delete=False) as stream:
        temporary = Path(stream.name)
        try:
            with urllib.request.urlopen(artifact["url"], timeout=60) as response:
                count = 0
                while chunk := response.read(1024 * 1024):
                    count += len(chunk)
                    if count > artifact["size"]:
                        raise ValueError("Downloaded artifact exceeds its pinned size.")
                    stream.write(chunk)
            stream.flush()
            if count != artifact["size"] or sha256(temporary) != artifact["sha256"]:
                raise ValueError("Downloaded artifact does not match its SHA-256/size pin.")
            # Cache is build-only; refuse overwriting anything created meanwhile.
            os.link(temporary, target)
        finally:
            temporary.unlink(missing_ok=True)
    return target


def isolated_environment(scratch: Path, epoch: int) -> dict[str, str]:
    # In particular: no PYTHON*, CONDA*, VIRTUAL_ENV, DYLD*, HF_TOKEN,
    # developer PATH, ~/.config/pip, or working-tree import path.
    return {
        "PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "LANG": "en_US.UTF-8",
        "TMPDIR": str(scratch), "SOURCE_DATE_EPOCH": str(epoch),
        "PIP_CONFIG_FILE": os.devnull, "PIP_DISABLE_PIP_VERSION_CHECK": "1",
        "HF_HUB_OFFLINE": "1", "TRANSFORMERS_OFFLINE": "1",
        "HF_HUB_DISABLE_IMPLICIT_TOKEN": "1", "HF_HUB_DISABLE_TELEMETRY": "1",
    }


def run(python: Path, args: list[str], *, cwd: Path, env: dict[str, str]) -> None:
    subprocess.run([str(python), "-I", "-B", *args], check=True, cwd=cwd, env=env,
                   stdout=sys.stderr, stderr=sys.stderr)


def install(python: Path, wheels: list[dict], cache: Path, target: Path,
            scratch: Path, env: dict[str, str], label: str) -> None:
    requirements = scratch / f"{label}-requirements.txt"
    requirements.write_text("".join(
        f"{wheel['name']}=={wheel['version']} --hash=sha256:{wheel['sha256']}\n"
        for wheel in wheels
    ))
    run(python, ["-m", "pip", "--isolated", "install", "--no-index", "--no-deps",
                 "--no-compile", "--require-hashes", "--disable-pip-version-check",
                 "--find-links", str(cache), "--target", str(target),
                 "-r", str(requirements)], cwd=scratch, env=env)


def relocate_whisper_dependencies(site: Path) -> list[dict]:
    """Remove only the pinned SciPy wheel's three unused build-machine RPATHs.

    Its actual BLAS/Fortran dependencies are already wheel-relative. Preserving
    Homebrew search directories would violate the standalone bundle contract.
    """
    import importlib.util
    check_path = Path(__file__).with_name("check_meeting_worker_bundle.py")
    spec = importlib.util.spec_from_file_location("forum_bundle_macho", check_path)
    checker = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(checker)
    target = site / "scipy/linalg/_fblas.cpython-312-darwin.so"
    before = sha256(target)
    known = {
        "/opt/homebrew/Cellar/gcc@13/13.4.0/lib/gcc/13/gcc/aarch64-apple-darwin23/13",
        "/opt/homebrew/Cellar/gcc@13/13.4.0/lib/gcc/13/gcc",
        "/opt/homebrew/Cellar/gcc@13/13.4.0/lib/gcc/13",
    }
    actual = set(checker.macho_arm64(target)["rpaths"])
    if not known.issubset(actual):
        raise ValueError("Pinned SciPy wheel changed its expected relocation inputs.")
    command = ["/usr/bin/install_name_tool"]
    for path in sorted(known):
        command.extend(["-delete_rpath", path])
    subprocess.run([*command, str(target)], check=True, stdout=sys.stderr, stderr=sys.stderr)
    subprocess.run(["/usr/bin/codesign", "--force", "--sign", "-", "--timestamp=none", str(target)],
                   check=True, stdout=sys.stderr, stderr=sys.stderr)
    after = sha256(target)
    record = site / "scipy-1.18.1.dist-info/RECORD"
    with record.open(newline="") as stream:
        rows = list(csv.reader(stream))
    relative = str(target.relative_to(site))
    matches = [row for row in rows if row[0] == relative]
    if len(matches) != 1:
        raise ValueError("Pinned SciPy RECORD is missing the relocated library.")
    matches[0][1:] = ["sha256=" + base64.urlsafe_b64encode(bytes.fromhex(after)).decode().rstrip("="),
                      str(target.stat().st_size)]
    with record.open("w", newline="") as stream:
        csv.writer(stream).writerows(rows)
    return [{"file": relative, "original_sha256": before, "bundled_sha256": after,
             "removed_build_rpaths": sorted(known), "signature": "ad-hoc, timestamp disabled"}]


def package(output: Path, cache: Path, offline: bool = False, with_asr: bool = False) -> dict:
    output = new_output(output)
    if cache.is_symlink():
        raise ValueError("Cache directory must not be a symlink.")
    cache.mkdir(parents=True, exist_ok=True)
    cache = cache.resolve(strict=True)
    lock_path = REPO / "services/asr-worker/runtime-macos-arm64.lock.json" if with_asr else LOCK
    lock = json.loads(lock_path.read_text())
    if lock["schema_version"] != 1 or lock["target"] != "aarch64-apple-darwin":
        raise ValueError("Unsupported runtime lock target/schema.")
    artifacts = [lock["python"], lock["python_license_archive"], *lock["wheels"]]
    for artifact in artifacts:
        validate_artifact(artifact)
    downloads = {artifact["filename"]: fetch(artifact, cache, offline) for artifact in artifacts}
    # Only this fresh, random staging directory is recursively cleaned up.
    # Existing output, model directories, user caches and other apps are never
    # removed. A failed build leaves the requested output path untouched.
    with tempfile.TemporaryDirectory(prefix=".forum-worker-build-", dir=output.parent) as directory:
        scratch = Path(directory)
        staged = scratch / "meeting-worker"
        staged.mkdir()
        env = isolated_environment(scratch, lock["source_date_epoch"])
        with tarfile.open(downloads[lock["python"]["filename"]], "r:gz") as archive:
            if any(not (member.name == "python" or member.name.startswith("python/"))
                   for member in archive.getmembers()):
                raise ValueError("Standalone archive has an unexpected top-level directory.")
            archive.extractall(staged, filter="data")
        python = staged / "python/bin/python3.12"
        if not python.is_file():
            raise ValueError("Pinned archive did not provide Python 3.12.")
        build_site = scratch / "build-site"
        install(python, [w for w in lock["wheels"] if w["role"] == "build"],
                cache, build_site, scratch, env, "build")
        source = scratch / "source"
        source.mkdir()
        source_hashes = {}
        sources = sorted((WORKER / "src").rglob("*.py"))
        sources += sorted((WORKER / "src/forum_meeting_worker/prompts").rglob("*.txt"))
        for original in [WORKER / "pyproject.toml", WORKER / "README.md", *sources]:
            if original.is_symlink():
                raise ValueError("Worker source files must not be symlinks.")
            relative = original.relative_to(WORKER)
            destination = source / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(original, destination)
            source_hashes[str(relative)] = sha256(original)
        wheelhouse = scratch / "wheelhouse"
        wheelhouse.mkdir()
        run(python, ["-c", "import sys; sys.path.insert(0, sys.argv[1]); "
                     "from setuptools.build_meta import build_wheel; build_wheel(sys.argv[2])",
                     str(build_site), str(wheelhouse)], cwd=source, env=env)
        worker_wheel, = wheelhouse.glob("forum_meeting_worker-*.whl")
        worker_version = worker_wheel.name.split("-")[1]
        runtime_site = staged / "python/lib/python3.12/site-packages"
        install(python, [w for w in lock["wheels"] if w["role"] == "runtime"],
                cache, runtime_site, scratch, env, "runtime")
        shutil.rmtree(runtime_site / "bin", ignore_errors=False)
        install(python, [{"name": "forum-meeting-worker", "version": worker_version,
                          "sha256": sha256(worker_wheel)}],
                wheelhouse, runtime_site, scratch, env, "worker")
        run(python, ["-m", "pip", "--isolated", "check"], cwd=scratch, env=env)
        native_relocations = relocate_whisper_dependencies(runtime_site) if with_asr else []
        # pip --target console scripts have staging-directory shebangs. Runtime
        # uses only our relative launcher plus '-m'; remove these unused scripts
        # and standalone's build-time pip from our owned staging tree.
        for item in (runtime_site / "bin", *runtime_site.glob("pip*")):
            if item.is_dir() and not item.is_symlink():
                shutil.rmtree(item)
            elif item.exists() or item.is_symlink():
                item.unlink()
        for item in (staged / "python/bin").iterdir():
            if item.name not in {"python", "python3", "python3.12"}:
                item.unlink()
        # pip --target records console wrappers with their staging shebang
        # hashes. Those wrappers were intentionally removed above; retaining
        # their RECORD rows both describes absent files and breaks repeatable
        # builds. Keep every actual module/license record unchanged.
        for record in runtime_site.glob("*.dist-info/RECORD"):
            with record.open(newline="") as stream:
                rows = [row for row in csv.reader(stream) if not row[0].startswith("../../bin/")]
            with record.open("w", newline="") as stream:
                csv.writer(stream).writerows(rows)
        shutil.rmtree(staged / "python/lib/python3.12/ensurepip")
        # install_only omits licenses for statically linked CPython libraries.
        # Read ONLY upstream metadata/license texts from the hash-pinned full
        # archive. The decompressor is a pinned build-only wheel, not a
        # Homebrew/system zstd executable or an installed runtime dependency.
        license_archive = downloads[lock["python_license_archive"]["filename"]]
        license_tar = scratch / "python-full.tar"
        run(python, ["-c", "import sys; sys.path.insert(0,sys.argv[1]); import zstandard; "
                     "source=open(sys.argv[2],'rb'); destination=open(sys.argv[3],'wb'); "
                     "zstandard.ZstdDecompressor().copy_stream(source,destination); destination.close()",
                     str(build_site), str(license_archive), str(license_tar)], cwd=scratch, env=env)
        third_party = staged / "third-party/python"
        third_party.mkdir(parents=True)
        with tarfile.open(license_tar, "r:") as archive:
            license_names = [member.name for member in archive.getmembers()
                             if member.name.startswith("python/licenses/") and member.isfile()]
            if not license_names:
                raise ValueError("Standalone full archive contains no upstream license files.")
            for name in ["python/PYTHON.json", *license_names]:
                if ".." in Path(name).parts or Path(name).name in {"", ".", ".."}:
                    raise ValueError("Unexpected upstream license path.")
                member = archive.getmember(name)
                if not member.isfile() or member.size > 2 * 1024 * 1024:
                    raise ValueError("Expected a bounded upstream metadata/license text file.")
                with archive.extractfile(member) as source_file:
                    (third_party / Path(name).name).write_bytes(source_file.read())
        notices = ["# AI Vision Forum meeting worker: third-party inventory", "",
                   "Pinned inputs and hashes: runtime-manifest.json. No model weights are included.",
                   "", "## Standalone CPython and statically linked libraries", "",
                   "Upstream build metadata and license texts: third-party/python/.",
                   lock["python_license_archive"]["metadata_url"], "",
                   "The full upstream license set is preserved; this inventory does not relicense upstream components.",
                   "", "## Runtime Python wheels", ""]
        for wheel in lock["wheels"]:
            if wheel["role"] == "runtime":
                notices.append(f"- {wheel['name']} {wheel['version']}: {wheel['metadata_url']}; "
                               "license/NOTICE files remain under python/lib/python3.12/site-packages in the upstream wheel layout.")
        (staged / "THIRD_PARTY.md").write_text("\n".join(notices) + "\n")
        checks = staged / "checks"
        checks.mkdir()
        check_source = Path(__file__).with_name("check_meeting_worker_bundle.py")
        shutil.copyfile(check_source, checks / check_source.name)
        launcher_dir = staged / "bin"
        launcher_dir.mkdir()
        launcher = launcher_dir / "meeting-worker"
        launcher.write_text('#!/bin/sh\nset -eu\n'
                            'worker_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)\n'
                            'unset PYTHONHOME PYTHONPATH VIRTUAL_ENV CONDA_PREFIX DYLD_LIBRARY_PATH DYLD_FALLBACK_LIBRARY_PATH\n'
                            'exec "$worker_root/python/bin/python3.12" -I -B -m forum_meeting_worker "$@"\n')
        launcher.chmod(0o755)
        contracts = staged / "contracts"
        contracts.mkdir()
        schema_source = REPO / "packages/contracts/forum.schema.json"
        schema_hash = None
        if schema_source.is_file():
            shutil.copyfile(schema_source, contracts / schema_source.name)
            schema_hash = sha256(schema_source)
        asr_manifest = None
        if with_asr:
            asr_directory = staged / "asr"
            asr_directory.mkdir()
            asr_source = REPO / "services/asr-worker/asr_worker.py"
            shutil.copyfile(asr_source, asr_directory / "asr_worker.py")
            asr_launcher = launcher_dir / "asr-worker"
            asr_launcher.write_text('#!/bin/sh\nset -eu\n'
                'worker_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)\n'
                'unset PYTHONHOME PYTHONPATH VIRTUAL_ENV CONDA_PREFIX DYLD_LIBRARY_PATH DYLD_FALLBACK_LIBRARY_PATH\n'
                'exec "$worker_root/python/bin/python3.12" -I -B "$worker_root/asr/asr_worker.py" "$@"\n')
            asr_launcher.chmod(0o755)
            asr_manifest = {"protocol_version": 1, "entrypoint": "bin/asr-worker",
                            "script_sha256": sha256(asr_source), "model_weights_included": False}
        translation_directory = staged / "translation"
        translation_directory.mkdir()
        translation_source = REPO / "services/translation-worker/translation_worker.py"
        shutil.copyfile(translation_source, translation_directory / "translation_worker.py")
        translation_manifest = {"protocol_version": 1, "script": "translation/translation_worker.py",
                                "script_sha256": sha256(translation_source), "model_weights_included": False}
        analysis_prompts = analysis_prompt_manifest(staged, source_hashes)
        manifest = {
            "owner": OWNER, "schema_version": 1, "target": lock["target"],
            "minimum_macos": lock["minimum_macos"], "python": lock["python"],
            "worker_version": worker_version, "worker_wheel_sha256": sha256(worker_wheel),
            "source_sha256": source_hashes, "lock_sha256": sha256(lock_path),
            "runtime_wheels": [w for w in lock["wheels"] if w["role"] == "runtime"],
            "build_wheels": [w for w in lock["wheels"] if w["role"] == "build"],
            "python_license_archive": lock["python_license_archive"],
            "python_license_files": [Path(name).name for name in license_names],
            "check_script_sha256": sha256(check_source),
            "packaging_script_sha256": sha256(Path(__file__)),
            "entrypoint": "bin/meeting-worker", "analysis_task_types": ANALYSIS_TASK_TYPES,
            "formal_product_acceptance": "not_evaluated", "analysis_prompts": analysis_prompts,
            "probe": "--allow-model-probe / diagnostics.model_probe (F01 only)",
            "translation_adapter": translation_manifest, "asr_adapter": asr_manifest, "contract_schema_sha256": schema_hash,
            "native_relocations": native_relocations,
        }
        (staged / "runtime-manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
        # Same-volume rename publishes a complete candidate. Never replace even
        # an empty directory belonging to the caller.
        if output.exists() or output.is_symlink():
            raise ValueError("Output appeared while building; refusing replacement.")
        staged.rename(output)
    return {"output": str(output), "launcher": str(output / "bin/meeting-worker"),
            "python_version": lock["python"]["version"], "minimum_macos": lock["minimum_macos"],
            "lock_sha256": sha256(lock_path)}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path, help="new absolute meeting-worker resource directory")
    parser.add_argument("--cache", required=True, type=Path, help="build artifact cache (no model weights)")
    parser.add_argument("--offline", action="store_true", help="only use already hash-verified cached artifacts")
    parser.add_argument("--with-asr", action="store_true", help="bundle the separately run Qwen3-ASR adapter and pinned dependencies")
    args = parser.parse_args()
    if sys.version_info < (3, 12) or platform.system() != "Darwin" or platform.machine() != "arm64":
        parser.error("Build on macOS arm64 with a Python 3.12+ bootstrap interpreter.")
    try:
        result = package(args.output, args.cache, args.offline, args.with_asr)
    except (ValueError, OSError, subprocess.CalledProcessError, tarfile.TarError) as exc:
        print(f"Worker packaging failed: {exc}", file=sys.stderr)
        return 1
    print(json.dumps(result, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
