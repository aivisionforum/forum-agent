"""Explicit F01 packaging diagnostic, never an analysis job or queue.

This synchronous call blocks this diagnostic process until completion. The
caller must apply a process deadline; production scheduling/cancellation is
deliberately absent. Only an existing local Qwen3 model is accepted.
"""

from __future__ import annotations

from contextlib import contextmanager
import ctypes
import gc
from importlib.metadata import version
import json
import logging
import os
from pathlib import Path
import sys
import time

LOG = logging.getLogger("forum_meeting_worker.probe")


class ProbeError(Exception):
    def __init__(self, code: str, message: str, number: int = -32602) -> None:
        super().__init__(message)
        self.code = code
        self.number = number


def validate_probe(params: object, *, allowed_model_types=("qwen3",)) -> tuple[Path, str, int]:
    if not isinstance(params, dict) or set(params) != {"model_path", "prompt", "max_tokens"}:
        raise ProbeError("INVALID_PARAMS", "Probe requires model_path, prompt and max_tokens.")
    prompt, limit = params["prompt"], params["max_tokens"]
    if not isinstance(prompt, str) or not prompt.strip() or len(prompt) > 4096:
        raise ProbeError("INVALID_PARAMS", "Probe prompt must contain 1 to 4096 characters.")
    if type(limit) is not int or not 1 <= limit <= 128:
        raise ProbeError("INVALID_PARAMS", "Probe max_tokens must be an integer from 1 to 128.")
    if not isinstance(params["model_path"], str):
        raise ProbeError("LOCAL_MODEL_REQUIRED", "An absolute existing local model directory is required.")
    path = Path(params["model_path"])
    if not path.is_absolute() or not path.is_dir():
        raise ProbeError("LOCAL_MODEL_REQUIRED", "An absolute existing local model directory is required.")
    path = path.resolve()
    try:
        for name in ("config.json", "tokenizer.json", "tokenizer_config.json"):
            if not (path / name).is_file() or (path / name).stat().st_size == 0:
                raise ValueError("Missing local model metadata.")
        config = json.loads((path / "config.json").read_text())
        if config.get("model_type") not in allowed_model_types or "model_file" in config or "auto_map" in config:
            raise ValueError("Only built-in Qwen3 architecture is allowed.")
        tokenizer = json.loads((path / "tokenizer_config.json").read_text())
        if "auto_map" in tokenizer:
            raise ValueError("Custom tokenizer code is not allowed.")
        index = path / "model.safetensors.index.json"
        if index.exists():
            weight_map = json.loads(index.read_text())["weight_map"]
            if not isinstance(weight_map, dict) or not weight_map:
                raise ValueError("Empty weight index.")
            names = set(weight_map.values())
        else:
            names = {"model.safetensors"}
        for name in names:
            # HF snapshot symlinks to existing blobs are allowed for read-only
            # inference. Never resolve a model identifier through the Hub.
            if (not isinstance(name, str) or Path(name).name != name
                    or not name.startswith("model") or not name.endswith(".safetensors")
                    or not (path / name).is_file() or (path / name).stat().st_size == 0):
                raise ValueError("Incomplete or non-local weights.")
    except (OSError, ValueError, KeyError, TypeError, AttributeError):
        raise ProbeError("LOCAL_MODEL_INVALID", "Expected complete local Qwen3 weights and built-in tokenizer; no custom code.") from None
    return path, prompt, limit


@contextmanager
def library_stdout_to_stderr():
    """Protect NDJSON even when a native dependency writes directly to fd 1."""
    libc = ctypes.CDLL(None)
    libc.fflush.argtypes = [ctypes.c_void_p]
    libc.fflush.restype = ctypes.c_int
    sys.stdout.flush()
    libc.fflush(None)
    saved = os.dup(1)
    try:
        os.dup2(2, 1)
        yield
    finally:
        try:
            sys.stdout.flush()
            # C stdio can keep printf output buffered until process exit;
            # flush it while fd 1 still points to stderr.
            libc.fflush(None)
        finally:
            os.dup2(saved, 1)
            os.close(saved)


def model_probe(params: object) -> dict:
    path, prompt, limit = validate_probe(params)
    # Set before importing HF/transformers; no token or developer config is
    # needed for an absolute local snapshot. No weights are downloaded.
    os.environ["HF_HUB_OFFLINE"] = "1"
    os.environ["TRANSFORMERS_OFFLINE"] = "1"
    os.environ["HF_HUB_DISABLE_TELEMETRY"] = "1"
    os.environ["HF_HUB_DISABLE_IMPLICIT_TOKEN"] = "1"
    started = time.monotonic()
    try:
        with library_stdout_to_stderr():
            import mlx.core as mx
            from mlx_lm import load, stream_generate
            from mlx_lm.sample_utils import make_sampler

            model, tokenizer = load(str(path), tokenizer_config={
                "trust_remote_code": False, "local_files_only": True, "token": False,
            })
            loaded = time.monotonic()
            rendered = tokenizer.apply_chat_template(
                [{"role": "user", "content": prompt}], tokenize=False,
                add_generation_prompt=True, enable_thinking=False,
            )
            text, first_token, last = [], None, None
            for response in stream_generate(
                model, tokenizer, rendered, max_tokens=limit, sampler=make_sampler(temp=0.0),
            ):
                if first_token is None:
                    first_token = time.monotonic()
                text.append(response.text)
                last = response
            if last is None:
                raise RuntimeError("Model emitted no generation response.")
            result = {
                "diagnostic_only": True, "model_path": str(path), "text": "".join(text),
                "python_version": sys.version.split()[0], "python_executable": sys.executable,
                "mlx_version": version("mlx"), "mlx_lm_version": version("mlx-lm"),
                "model_load_seconds": loaded - started,
                "first_token_seconds": first_token - started,
                "total_seconds": time.monotonic() - started,
                "generation_tokens": last.generation_tokens,
                "generation_tokens_per_second": last.generation_tps,
                "peak_memory_gb": last.peak_memory,
                "finish_reason": last.finish_reason,
            }
            del model, tokenizer
            gc.collect()
            mx.clear_cache()
            return result
    except Exception as exc:
        LOG.error("local model diagnostic failed (%s)", type(exc).__name__)
        raise ProbeError("MODEL_PROBE_FAILED", "Local MLX diagnostic failed; see stderr for error type.", -32010) from None
