#!/usr/bin/env python3
"""Check a packaged sidecar under an isolated subprocess environment.

No model is imported unless --model names an existing local Qwen3 snapshot.
This is local isolation evidence, not validation on a second clean Mac.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import struct
import subprocess
import sys
import tempfile
import uuid

OWNER = "ai-vision-forum-meeting-worker-v1"
ANALYSIS_TASK_TYPES = ["insight", "minutes", "event_report", "suggested_questions",
                       "redaction_review", "closing_brief"]
PROMPT_DIRECTORY = Path("python/lib/python3.12/site-packages/forum_meeting_worker/prompts/v1")
DEFAULT_PROMPT = "请用一句中文概括：今天的圆桌讨论决定先完成实时翻译，再验证会议摘要。"
RUNTIME_INFO = """
import importlib.metadata as m, json, platform, ssl, sys
from pathlib import Path
root=Path(sys.argv[1]).resolve()
paths=[str(Path(p).resolve()) for p in sys.path]
if not all(Path(p).is_relative_to(root) for p in paths):
    raise SystemExit('sys.path escapes the standalone runtime')
if not Path(sys.executable).resolve().is_relative_to(root):
    raise SystemExit('interpreter escapes the standalone runtime')
print(json.dumps({'python_version':platform.python_version(),
 'executable':sys.executable,'prefix':sys.prefix,'base_prefix':sys.base_prefix,
 'sys_path':paths,'isolated':sys.flags.isolated,'no_user_site':sys.flags.no_user_site,
 'openssl':ssl.OPENSSL_VERSION,'machine':platform.machine(),
 'versions':{d.metadata['Name'].lower().replace('_','-'):d.version for d in m.distributions()}}))
"""
METAL_CHECK = """
import importlib.metadata as m, json, time
from forum_meeting_worker.model_probe import library_stdout_to_stderr
started=time.monotonic()
with library_stdout_to_stderr():
    import mlx.core as mx
    if not mx.metal.is_available():
        raise SystemExit('MLX Metal backend is unavailable')
    mx.set_default_device(mx.gpu)
    if mx.default_device().type != mx.gpu:
        raise SystemExit('MLX did not select the GPU')
    a=mx.array([[1.,2.],[3.,4.]],dtype=mx.float32)
    b=mx.array([[5.,6.],[7.,8.]],dtype=mx.float32)
    c=a@b
    mx.eval(c)
    mx.synchronize(mx.gpu)
    values=c.tolist()
    if values != [[19.,22.],[43.,50.]]:
        raise SystemExit('Metal matrix result is incorrect')
    result={'status':'passed','mlx_version':m.version('mlx'),
            'device':str(mx.default_device()),'device_info':mx.device_info(),
            'operation':'float32 2x2 matmul + mx.eval + mx.synchronize(gpu)',
            'actual':values,'seconds':time.monotonic()-started,'model_weights_loaded':False}
print(json.dumps(result))
"""


def isolated_environment(directory: Path) -> dict[str, str]:
    return {
        "PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "LANG": "en_US.UTF-8",
        "TMPDIR": str(directory), "HF_HOME": str(directory / "hf-cache"),
        "XDG_CACHE_HOME": str(directory / "xdg-cache"),
        "HF_HUB_OFFLINE": "1", "TRANSFORMERS_OFFLINE": "1",
        "HF_HUB_DISABLE_TELEMETRY": "1", "HF_HUB_DISABLE_IMPLICIT_TOKEN": "1",
        "TOKENIZERS_PARALLELISM": "false",
    }


def check_paths(root: Path) -> dict:
    manifest = json.loads((root / "runtime-manifest.json").read_text())
    if not isinstance(manifest, dict) or not isinstance(manifest.get("source_sha256"), dict):
        raise ValueError("Invalid runtime manifest or source digest map.")
    schema_hash = manifest.get("contract_schema_sha256")
    if schema_hash and hashlib.sha256((root / "contracts/forum.schema.json").read_bytes()).hexdigest() != schema_hash:
        raise ValueError("Bundled contract schema hash mismatch.")
    if (manifest.get("owner") != OWNER
            or manifest.get("analysis_task_types") != ANALYSIS_TASK_TYPES
            or manifest.get("formal_product_acceptance") != "not_evaluated"
            or "formal_job_capabilities" in manifest):
        raise ValueError("Manifest must distinguish six implemented analysis tasks from product acceptance not evaluated.")
    for path in root.rglob("*"):
        if path.is_symlink() and not path.resolve().is_relative_to(root):
            raise ValueError(f"Bundle symlink escapes its resource directory: {path.relative_to(root)}")
    prompts = manifest.get("analysis_prompts")
    if not isinstance(prompts, dict) or set(prompts) != set(ANALYSIS_TASK_TYPES):
        raise ValueError("Manifest must identify exactly six analysis prompts.")
    directory = root / PROMPT_DIRECTORY
    if directory.is_symlink() or not directory.is_dir() or {p.name for p in directory.iterdir()} != {f"{k}.txt" for k in ANALYSIS_TASK_TYPES}:
        raise ValueError("Bundle must contain exactly six v1 analysis prompt resources.")
    for kind in ANALYSIS_TASK_TYPES:
        item = prompts[kind]
        relative = PROMPT_DIRECTORY / f"{kind}.txt"
        if (not isinstance(item, dict) or set(item) != {"prompt_version", "relative_path", "sha256", "size_bytes"}
                or item["relative_path"] != str(relative) or item["prompt_version"] != f"{kind}-v1"):
            raise ValueError("Invalid analysis prompt resource identity.")
        path = root / relative
        if path.is_symlink() or not path.is_file() or not 0 < path.stat().st_size <= 64 * 1024:
            raise ValueError("Analysis prompt is not a bounded regular file.")
        data = path.read_bytes()
        data.decode("utf-8")
        actual_hash = hashlib.sha256(data).hexdigest()
        source_key = f"src/forum_meeting_worker/prompts/v1/{kind}.txt"
        if (type(item["size_bytes"]) is not int or len(data) != item["size_bytes"]
                or actual_hash != item["sha256"]
                or manifest.get("source_sha256", {}).get(source_key) != actual_hash):
            raise ValueError("Bundled analysis prompt hash/size/source mismatch.")
    return manifest


def lifecycle_requests(scratch: Path, manifest: dict) -> list[dict]:
    """No grants or input files: both negative job checks must refuse early."""
    profile_version = "bundle-interface-check-v1"
    snapshot_id, session_id, job_id = (str(uuid.uuid4()) for _ in range(3))
    config = {"model_profile": "meeting-8b-v1", "model_manifest_id": "sha256:" + "0" * 64,
              "prompt_version": "minutes-v1", "prompt_sha256": manifest["analysis_prompts"]["minutes"]["sha256"],
              "profile_id": "ai-vision-forum", "profile_version": profile_version,
              "profile_sha256": hashlib.sha256(b"synthetic bundle check profile").hexdigest(),
              "projection_policy_hash": hashlib.sha256(b"synthetic bundle check policy").hexdigest(),
              "generation": {"temperature": 0.0, "max_output_tokens": 1024, "safety_tokens": 128,
                             "max_retries": 1, "context_limit": 8192}}
    config["effective_config_hash"] = hashlib.sha256(json.dumps(config, sort_keys=True, ensure_ascii=False,
                                                               separators=(",", ":")).encode()).hexdigest()
    ungranted = {"job_id": job_id, "attempt": 1, "kind": "minutes", "session_ids": [session_id],
                 "snapshot": {"id": snapshot_id, "relative_path": "input.json", "sha256": "0" * 64, "input_cursor": 0},
                 "model_profile": "meeting-8b-v1", "prompt_version": "minutes-v1", "remaining_budget_ms": 1000,
                 "config": config, "confirmed_checkpoints": {"relative_path": "checkpoints.json", "sha256": "0" * 64}}
    return [rpc("initialize", "init", {"protocol_version": 1, "instance_id": str(uuid.uuid4()),
             "job_root": str(scratch), "profile_id": "ai-vision-forum", "profile_version": profile_version}),
            rpc("health.ping", "ping"), rpc("jobs.run", "malformed", {"kind": "minutes"}),
            rpc("jobs.run", "ungranted", ungranted)]


def validate_lifecycle_replies(requests: list[dict], replies: list[dict]) -> dict:
    if len(replies) != len(requests) or any(
        not isinstance(reply, dict) or reply.get("jsonrpc") != "2.0" or reply.get("id") != req["id"]
        for req, reply in zip(requests, replies)
    ):
        raise ValueError("Worker response framing or request IDs do not match.")
    for reply in replies:
        if ("result" in reply) == ("error" in reply):
            raise ValueError("Worker response must contain exactly one result or error.")
        if "result" in reply and not isinstance(reply["result"], dict):
            raise ValueError("Worker lifecycle result must be an object.")
        if "error" in reply and (not isinstance(reply["error"], dict)
                                 or not isinstance(reply["error"].get("data"), dict)):
            raise ValueError("Worker lifecycle error must contain an object business code.")
    by_id = {reply["id"]: reply for reply in replies}
    if by_id["init"].get("result", {}).get("capabilities") != {
            "health": True, "cooperative_pause": True, "task_types": ANALYSIS_TASK_TYPES, "model_clients": ["local-mlx"]}:
        raise ValueError("Worker must advertise cooperative pause, its six task types and local-mlx client.")
    if by_id["ping"].get("result", {}).get("status") != "ok":
        raise ValueError("Worker health check failed.")
    actual_errors = {}
    for name, expected in (("malformed", "INVALID_PARAMS"), ("ungranted", "MODEL_UNAVAILABLE")):
        failure = by_id[name].get("error", {})
        actual = failure.get("data", {}).get("code")
        if (actual != expected or "result" in by_id[name]
                or type(failure.get("code")) is not int or failure["code"] >= 0
                or not isinstance(failure.get("message"), str)):
            raise ValueError(f"Worker did not safely reject {name} job: expected {expected}, got {actual!r}.")
        actual_errors[name] = actual
    if by_id["shutdown"].get("result", {}).get("status") != "shutting_down":
        raise ValueError("Worker shutdown failed.")
    return actual_errors


def macho_arm64(path: Path) -> dict:
    """Read Mach-O/FAT headers using stdlib; no Xcode/otool on a clean Mac."""
    arm64 = 0x0100000C
    size = path.stat().st_size
    with path.open("rb") as stream:
        magic = stream.read(4)
        offset, slice_size = 0, size
        fat_formats = {b"\xca\xfe\xba\xbe": (">", False), b"\xbe\xba\xfe\xca": ("<", False),
                       b"\xca\xfe\xba\xbf": (">", True), b"\xbf\xba\xfe\xca": ("<", True)}
        if magic in fat_formats:
            endian, fat64 = fat_formats[magic]
            count_bytes = stream.read(4)
            if len(count_bytes) != 4:
                raise ValueError("Truncated FAT header.")
            count, = struct.unpack(endian + "I", count_bytes)
            if not 1 <= count <= 64:
                raise ValueError("Invalid FAT architecture count.")
            entry_size = 32 if fat64 else 20
            slices = []
            for _ in range(count):
                entry = stream.read(entry_size)
                if len(entry) != entry_size:
                    raise ValueError("Truncated FAT architecture table.")
                fields = struct.unpack(endian + ("IIQQII" if fat64 else "IIIII"), entry)
                cpu, _, entry_offset, entry_bytes = fields[:4]
                if entry_offset < 8 + count * entry_size or entry_offset + entry_bytes > size:
                    raise ValueError("FAT slice escapes the native file.")
                if cpu == arm64:
                    slices.append((entry_offset, entry_bytes))
            if len(slices) != 1:
                raise ValueError(f"Expected one arm64 slice: {path.name}")
            offset, slice_size = slices[0]
            stream.seek(offset)
            magic = stream.read(4)
        if magic not in {b"\xcf\xfa\xed\xfe", b"\xfe\xed\xfa\xcf"}:
            raise ValueError(f"Expected a 64-bit Mach-O native file: {path.name}")
        endian = "<" if magic == b"\xcf\xfa\xed\xfe" else ">"
        rest = stream.read(28)
        if len(rest) != 28:
            raise ValueError("Truncated Mach-O header.")
        cpu, subtype, file_type, count, command_bytes, flags, reserved = struct.unpack(endian + "7I", rest)
        if cpu != arm64 or command_bytes > 16 * 1024 * 1024 or 32 + command_bytes > slice_size:
            raise ValueError("Invalid arm64 Mach-O header or command bounds.")
        commands = stream.read(command_bytes)
        if len(commands) != command_bytes:
            raise ValueError("Truncated Mach-O commands.")
    cursor, dependencies, rpaths, minimum = 0, [], [], None
    for _ in range(count):
        if cursor + 8 > len(commands):
            raise ValueError("Truncated Mach-O command header.")
        command, length = struct.unpack_from(endian + "II", commands, cursor)
        if length < 8 or length % 4 or cursor + length > len(commands):
            raise ValueError("Invalid Mach-O command size.")
        data = commands[cursor:cursor + length]
        if command in {0xC, 0x80000018, 0x8000001F, 0x80000023, 0x20, 0x8000001C}:
            if length < 12:
                raise ValueError("Truncated native loader command.")
            name_offset, = struct.unpack_from(endian + "I", data, 8)
            if not 12 <= name_offset < length or b"\0" not in data[name_offset:]:
                raise ValueError("Invalid native loader name offset.")
            name = data[name_offset:].split(b"\0", 1)[0].decode("utf-8")
            (rpaths if command == 0x8000001C else dependencies).append(name)
        # LC_ID_DYLIB describes the library itself; it is not loaded from its
        # install name. CPython legitimately has an /install/... dylib ID.
        if command in {0x32, 0x24}:
            if length < (24 if command == 0x32 else 16):
                raise ValueError("Truncated minimum OS command.")
            if command == 0x32:
                os_platform, packed = struct.unpack_from(endian + "II", data, 8)
                if os_platform != 1:
                    raise ValueError("Native dependency is not built for macOS.")
            else:
                packed, = struct.unpack_from(endian + "I", data, 8)
            minimum = (packed >> 16, (packed >> 8) & 255, packed & 255)
        cursor += length
    if cursor != len(commands) or minimum is None:
        raise ValueError("Missing OS version or inconsistent Mach-O command count.")
    return {"architecture": "arm64", "minimum_macos": minimum,
            "dependencies": dependencies, "rpaths": rpaths}


def check_native_dependencies(root: Path) -> dict:
    natives = sorted(set(root.rglob("*.so")) | set(root.rglob("*.dylib"))
                     | {root / "python/bin/python3.12"})
    audited = []
    for path in natives:
        native = macho_arm64(path)
        if native["minimum_macos"] > (14, 0, 0):
            raise ValueError(f"Native dependency requires a newer OS than macOS 14: {path.relative_to(root)}")
        for dependency in [*native["dependencies"], *native["rpaths"]]:
            is_rpath = dependency in native["rpaths"]
            normalized = os.path.normpath(dependency)
            system = normalized.startswith(("/System/Library/", "/usr/lib/")) or (
                is_rpath and normalized in {"/System/Library", "/usr/lib"})
            relative = dependency.startswith(("@rpath/", "@loader_path/", "@executable_path/"))
            exact_loader = is_rpath and dependency in {"@loader_path", "@executable_path"}
            if not (system or relative or exact_loader):
                raise ValueError(f"Non-portable loader path: {path.relative_to(root)} -> {dependency}")
            for prefix, base in (("@loader_path/", path.parent),
                                 ("@executable_path/", root / "python/bin")):
                if dependency.startswith(prefix):
                    if not (base / dependency.removeprefix(prefix)).resolve().is_relative_to(root):
                        raise ValueError("Relative native loader path escapes the bundle.")
        audited.append(str(path.relative_to(root)))
    return {"native_file_count": len(audited), "native_files": audited,
            "audit_tool": "Python stdlib Mach-O/FAT parser (no developer tools)",
            "architecture": "arm64", "maximum_minimum_macos": "14.0",
            "external_dependencies": "Apple system libraries or relative loader paths only"}


def rpc(method: str, request_id: str, params: dict | None = None) -> dict:
    return {"jsonrpc": "2.0", "id": request_id, "method": method, "params": params or {}}


def check(root: Path, model: Path | None, timeout: float, max_tokens: int) -> dict:
    root = root.resolve(strict=True)
    manifest = check_paths(root)
    native = check_native_dependencies(root)
    with tempfile.TemporaryDirectory(prefix="forum-worker-bundle-check-") as directory:
        scratch = Path(directory)
        env = isolated_environment(scratch)
        python = root / "python/bin/python3.12"
        info = subprocess.run([str(python), "-I", "-B", "-c", RUNTIME_INFO, str(root)],
                              check=True, capture_output=True, text=True, env=env, cwd=scratch, timeout=30)
        runtime = json.loads(info.stdout)
        if runtime["python_version"] != manifest["python"]["version"] or runtime["machine"] != "arm64":
            raise ValueError("Candidate interpreter does not match its pinned version/architecture.")
        expected = {w["name"]: w["version"] for w in manifest["runtime_wheels"]}
        expected["forum-meeting-worker"] = manifest["worker_version"]
        if runtime["versions"] != expected:
            raise ValueError("Installed packages do not exactly match the runtime manifest.")
        metal_process = subprocess.run([str(python), "-I", "-B", "-c", METAL_CHECK],
                                       capture_output=True, text=True, env=env, cwd=scratch, timeout=30)
        if metal_process.returncode != 0:
            raise ValueError(f"MLX Metal matrix check failed: {metal_process.stderr[-2000:]}")
        metal = json.loads(metal_process.stdout)
        if metal.get("status") != "passed":
            raise ValueError("MLX Metal matrix check did not pass.")
        asr = None
        if manifest.get("asr_adapter"):
            # Check both new model implementations from the candidate runtime;
            # no model weights or audio are loaded.
            checked = subprocess.run([str(python), "-I", "-B", "-c",
                "import mlx_qwen3_asr, mlx_lm.models.hunyuan_v1_dense, json; "
                "print(json.dumps({'imports': 'passed', 'models_loaded': False}))"],
                capture_output=True, text=True, env=env, cwd=scratch, timeout=60)
            if checked.returncode != 0:
                raise ValueError(f"Automatic ASR dependency import failed: {checked.stderr[-2000:]}")
            asr = json.loads(checked.stdout)
        for key, relative in [("asr_adapter", "asr/asr_worker.py"), ("translation_adapter", "translation/translation_worker.py")]:
            adapter = manifest.get(key)
            if adapter:
                script = root / relative
                if hashlib.sha256(script.read_bytes()).hexdigest() != adapter["script_sha256"]:
                    raise ValueError(f"{key} source differs from its manifest")
                compile(script.read_text(), str(script), "exec")
        requests = lifecycle_requests(scratch, manifest)
        args = [str(root / "bin/meeting-worker")]
        if model is not None:
            if not model.is_absolute() or not model.is_dir():
                raise ValueError("--model must be an existing absolute directory; model IDs are not accepted.")
            args.append("--allow-model-probe")
            requests.append(rpc("diagnostics.model_probe", "probe", {
                "model_path": str(model.resolve()), "prompt": DEFAULT_PROMPT, "max_tokens": max_tokens,
            }))
        requests.append(rpc("shutdown", "shutdown"))
        completed = subprocess.run(args, input="".join(json.dumps(r) + "\n" for r in requests),
                                   capture_output=True, text=True, env=env, cwd=scratch, timeout=timeout)
        if completed.returncode != 0:
            raise ValueError(f"Worker exited {completed.returncode}: {completed.stderr[-2000:]}")
        try:
            replies = [json.loads(line) for line in completed.stdout.splitlines()]
        except json.JSONDecodeError:
            raise ValueError("Worker stdout contains non-protocol output.") from None
        job_rejections = validate_lifecycle_replies(requests, replies)
        if model is not None:
            probe_reply = next(reply for reply in replies if reply["id"] == "probe")
            probe = probe_reply.get("result", {})
            if not probe.get("diagnostic_only") or not probe.get("text", "").strip():
                raise ValueError(f"Model diagnostic did not produce text: {json.dumps(probe_reply)}; {completed.stderr[-2000:]}")
        if replies[-1].get("result", {}).get("status") != "shutting_down":
            raise ValueError("Worker shutdown failed.")
        return {
            "status": "passed", "resource_directory": str(root),
            "validation_scope": "isolated subprocess on this Mac; NOT a second clean Mac acceptance",
            "host": {"macos": platform.mac_ver()[0], "architecture": platform.machine()},
            "environment_keys": sorted(env), "cwd": "new temporary directory outside source tree",
            "runtime": runtime, "native_dependency_audit": native,
            "metal_matrix_check": metal,
            "asr_adapter": asr,
            "protocol_stdout_only": True, "model_probe_requested": model is not None,
            "analysis_task_types": replies[0]["result"]["capabilities"]["task_types"],
            "formal_product_acceptance": "not_evaluated",
            "analysis_prompts": manifest["analysis_prompts"], "job_rejections": job_rejections,
            "responses": replies, "stderr": completed.stderr,
        }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    target = parser.add_mutually_exclusive_group(required=True)
    target.add_argument("--resource-dir", type=Path, help="meeting-worker resource directory")
    target.add_argument("--app", type=Path, help=".app containing Contents/Resources/meeting-worker")
    parser.add_argument("--model", type=Path, help="explicit existing local Qwen3 snapshot; never downloaded")
    parser.add_argument("--timeout", type=float, default=180, help="process deadline including model probe")
    parser.add_argument("--max-tokens", type=int, default=64, help="bounded diagnostic output (1..128)")
    parser.add_argument("--report", type=Path, help="write JSON evidence to a new file; never overwrite")
    args = parser.parse_args()
    root = args.resource_dir or args.app / "Contents/Resources/meeting-worker"
    if not math.isfinite(args.timeout) or args.timeout <= 0 or not 1 <= args.max_tokens <= 128:
        parser.error("--timeout must be positive; --max-tokens must be 1..128.")
    try:
        result = check(root, args.model, args.timeout, args.max_tokens)
        encoded = json.dumps(result, ensure_ascii=False, indent=2) + "\n"
        if args.report:
            with args.report.open("x") as stream:
                stream.write(encoded)
        print(encoded, end="")
    except (ValueError, OSError, subprocess.SubprocessError) as exc:
        print(f"Worker bundle check failed: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
