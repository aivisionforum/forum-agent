#!/usr/bin/env python3
"""Private, sequential PCM -> Whisper transcript adapter (protocol v1).

The owning Rust ASR node supplies deadlines and process-group containment.
No audio device, file decoder, network model resolver, or meeting DB is used.
"""
from __future__ import annotations

import argparse
import json
import math
import os
from pathlib import Path
import select
import sys
import time
from typing import BinaryIO
from uuid import UUID

PROTOCOL_VERSION = 1
MAX_FRAME_BYTES = 16 * 1024 * 1024
MAX_SAMPLES = 30 * 16000
FRAME_TIMEOUT_SECONDS = 15


class ProtocolError(ValueError):
    pass


class FrameReader:
    def __init__(self, stream: BinaryIO):
        self.fd = stream.fileno()
        self.pending = bytearray()

    def read(self, *, timeout: float = FRAME_TIMEOUT_SECONDS) -> dict | None:
        """Bound total frame assembly; retain bytes from a following request."""
        deadline = time.monotonic() + timeout if self.pending else None
        while True:
            newline = self.pending.find(b"\n")
            if newline >= 0:
                if newline > MAX_FRAME_BYTES:
                    raise ProtocolError("FRAME_TOO_LARGE")
                frame = bytes(self.pending[:newline])
                del self.pending[:newline + 1]
                break
            if len(self.pending) > MAX_FRAME_BYTES:
                raise ProtocolError("FRAME_TOO_LARGE")
            remaining = None if deadline is None else deadline - time.monotonic()
            if remaining is not None and remaining <= 0:
                raise ProtocolError("FRAME_TIMEOUT")
            if not select.select([self.fd], [], [], remaining)[0]:
                raise ProtocolError("FRAME_TIMEOUT")
            chunk = os.read(self.fd, min(65536, MAX_FRAME_BYTES + 1 - len(self.pending)))
            if not chunk:
                if self.pending:
                    raise ProtocolError("TRUNCATED_FRAME")
                return None
            if deadline is None:
                deadline = time.monotonic() + timeout
            self.pending.extend(chunk)
        try:
            value = json.loads(frame, parse_constant=lambda _: (_ for _ in ()).throw(ProtocolError("NON_FINITE_JSON")))
        except (ValueError, UnicodeError, RecursionError) as exc:
            raise ProtocolError("INVALID_JSON") from exc
        if not isinstance(value, dict):
            raise ProtocolError("EXPECTED_OBJECT")
        return value


def validate_request(value: dict) -> tuple[str, list[float], str]:
    if set(value) != {"id", "samples", "sample_rate", "language"}:
        raise ProtocolError("INVALID_FIELDS")
    request_id = value["id"]
    try:
        if not isinstance(request_id, str) or UUID(request_id).int == 0:
            raise ValueError()
    except (ValueError, AttributeError):
        raise ProtocolError("INVALID_ID") from None
    if type(value["sample_rate"]) is not int or value["sample_rate"] != 16000:
        raise ProtocolError("INVALID_SAMPLE_RATE")
    if value["language"] not in {"auto", "zh", "en"}:
        raise ProtocolError("UNSUPPORTED_LANGUAGE")
    samples = value["samples"]
    if not isinstance(samples, list) or not 0 < len(samples) <= MAX_SAMPLES:
        raise ProtocolError("INVALID_SAMPLE_COUNT")
    if any(type(x) not in (int, float) or not math.isfinite(x) or abs(x) > 1 for x in samples):
        raise ProtocolError("INVALID_SAMPLE")
    return request_id, samples, value["language"]


def local_model(path: Path) -> Path:
    if not path.is_absolute() or ".." in path.parts:
        raise ValueError("MODEL_REQUIRES_ABSOLUTE_LOCAL_PATH")
    resolved = path.resolve(strict=True)
    if not resolved.is_dir():
        raise ValueError("MODEL_DIRECTORY_REQUIRED")
    config = resolved / "config.json"
    weights = resolved / "weights.safetensors"
    # Existing HF cache blobs may be symlinks; only read those files. A model
    # name, missing directory, pickle/npz fallback or custom module is refused.
    if not config.is_file() or not 0 < config.stat().st_size <= 1024 * 1024:
        raise ValueError("MODEL_CONFIG_MISSING")
    if not weights.is_file() or weights.stat().st_size <= 0:
        raise ValueError("MODEL_SAFETENSORS_MISSING")
    return resolved


class WhisperEngine:
    def __init__(self, path: Path):
        self.path = local_model(path)
        os.environ.update(HF_HUB_OFFLINE="1", TRANSFORMERS_OFFLINE="1",
                          HF_HUB_DISABLE_IMPLICIT_TOKEN="1", HF_HUB_DISABLE_TELEMETRY="1")
        import mlx.core as mx
        import mlx_whisper
        import numpy as np
        from mlx_whisper.transcribe import ModelHolder
        self.mx, self.whisper, self.np = mx, mlx_whisper, np
        model = ModelHolder.get_model(str(self.path), mx.float16)
        mx.eval(model.parameters())
        mx.synchronize()

    def transcribe(self, samples: list[float], language: str) -> tuple[str, str | None]:
        # All-zero input contains no acoustic evidence. This avoids the known
        # empty-input hallucination without filtering quiet speech by volume.
        if not any(samples):
            return "", None
        result = self.whisper.transcribe(
            self.np.asarray(samples, dtype=self.np.float32), path_or_hf_repo=str(self.path),
            language=None if language == "auto" else language, task="transcribe",
            temperature=0.0, condition_on_previous_text=False, verbose=None,
        )
        text = result.get("text")
        detected = result.get("language")
        if not isinstance(text, str) or (detected is not None and not isinstance(detected, str)):
            raise ValueError("INVALID_MODEL_RESULT")
        # Whisper returns one dominant language, not spans for code switching.
        return text, detected


def response(engine, request: dict) -> dict:
    request_id, samples, language = validate_request(request)
    try:
        text, detected = engine.transcribe(samples, language)
        if len(text.encode("utf-8")) > 256 * 1024:
            raise ValueError("MODEL_RESULT_TOO_LARGE")
        return {"id": request_id, "text": text, "detected_language": detected,
                "status": "success" if text.strip() else "empty", "error": None}
    except Exception:
        # Never pass raw exception text (which can include a transcript or
        # request contents) into UI logs. The segment remains failed, not empty.
        return {"id": request_id, "text": "", "detected_language": None,
                "status": "failed", "error": "ASR_INFERENCE_FAILED"}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", required=True, type=Path)
    args = parser.parse_args()
    # Keep a dedicated protocol FD; Python/C/MLX output now goes to stderr.
    sys.stdout.flush()
    with os.fdopen(os.dup(sys.stdout.fileno()), "w", buffering=1, encoding="utf-8") as protocol:
        os.dup2(sys.stderr.fileno(), sys.stdout.fileno())
        def emit(value):
            protocol.write(json.dumps(value, ensure_ascii=False, allow_nan=False) + "\n")
            protocol.flush()
        try:
            engine = WhisperEngine(args.model)
        except Exception:
            emit({"type": "error", "error": "ASR_MODEL_LOAD_FAILED"})
            return 1
        emit({"type": "ready", "protocol_version": PROTOCOL_VERSION,
              "backend": "mlx-whisper", "model_path": str(engine.path)})
        try:
            reader = FrameReader(sys.stdin.buffer)
            while (request := reader.read()) is not None:
                if request == {"type": "shutdown"}:
                    return 0
                emit(response(engine, request))
        except ProtocolError as exc:
            emit({"type": "error", "error": str(exc)})
            return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
