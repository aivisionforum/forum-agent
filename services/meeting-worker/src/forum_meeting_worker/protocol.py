"""Strict, bounded NDJSON transport and the F01 lifecycle contract.

This module performs no model loading, network requests, or database writes.
Only the host may choose job paths and later grant model capabilities.
"""

from __future__ import annotations

import json
import math
import os
from pathlib import Path
import selectors
import time
from typing import BinaryIO, Iterator
from uuid import UUID

from . import __version__

PROTOCOL_VERSION = 1
MAX_FRAME_BYTES = 1024 * 1024  # Bytes before LF, including any CR.


class TransportError(Exception):
    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code = code


def frames(stream: BinaryIO, timeout_seconds: float) -> Iterator[bytes]:
    """Read stdin without unbounded lines or indefinitely incomplete frames.

    Idle connections have no timeout. A deadline starts at the first byte of
    each frame and does not reset for slowly arriving bytes. A framing error
    closes this connection; attempting recovery could interpret a frame tail
    as a command. The F01 transport targets the macOS/POSIX sidecar pipe.
    """
    pending = bytearray()
    started: float | None = None
    with selectors.DefaultSelector() as selector:
        selector.register(stream, selectors.EVENT_READ)
        while True:
            remaining = None
            if started is not None:
                remaining = timeout_seconds - (time.monotonic() - started)
                if remaining <= 0:
                    raise TransportError("FRAME_TIMEOUT", "Incomplete frame timed out.")
            if not selector.select(remaining):
                raise TransportError("FRAME_TIMEOUT", "Incomplete frame timed out.")
            chunk = os.read(stream.fileno(), 65536)
            if not chunk:
                if pending:
                    raise TransportError("INCOMPLETE_FRAME", "Expected newline before EOF.")
                return
            if started is None:
                started = time.monotonic()
            pending.extend(chunk)
            while (newline := pending.find(b"\n")) >= 0:
                if newline > MAX_FRAME_BYTES:
                    raise TransportError("FRAME_TOO_LARGE", "Frame exceeds 1 MiB.")
                frame = bytes(pending[:newline])
                del pending[:newline + 1]
                started = time.monotonic() if pending else None
                yield frame
            if len(pending) > MAX_FRAME_BYTES:
                raise TransportError("FRAME_TOO_LARGE", "Frame exceeds 1 MiB.")


def error(request_id: object, number: int, code: str, message: str) -> dict:
    return {
        "jsonrpc": "2.0",
        "id": request_id,
        "error": {"code": number, "message": message, "data": {"code": code}},
    }


def _object_without_duplicates(pairs: list[tuple[str, object]]) -> dict:
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("Duplicate JSON field.")
        result[key] = value
    return result


def _reject_non_json_number(value: str) -> None:
    raise ValueError("Non-JSON number.")


def _valid_id(value: object) -> bool:
    return (value is None or isinstance(value, str) or type(value) is int
            or (type(value) is float and math.isfinite(value)))


class Protocol:
    """One initialized host connection, with no analysis work queue yet."""

    def __init__(self, *, allow_model_probe: bool = False) -> None:
        self.configuration: dict | None = None
        self.stopping = False
        self.allow_model_probe = allow_model_probe

    def handle(self, frame: bytes) -> dict | None:
        try:
            request = json.loads(
                frame.decode("utf-8"),
                object_pairs_hook=_object_without_duplicates,
                parse_constant=_reject_non_json_number,
            )
        except (UnicodeDecodeError, ValueError, RecursionError):
            return error(None, -32700, "PARSE_ERROR", "Invalid UTF-8 JSON frame.")

        if not isinstance(request, dict):
            return error(None, -32600, "INVALID_REQUEST", "Expected one JSON-RPC object.")
        request_id = request.get("id")
        if (
            request.get("jsonrpc") != "2.0"
            or not isinstance(request.get("method"), str)
            or not request["method"]
            or ("id" in request and not _valid_id(request_id))
            or ("params" in request and not isinstance(request["params"], (dict, list)))
        ):
            return error(None, -32600, "INVALID_REQUEST", "Invalid JSON-RPC request.")

        response = self._dispatch(request_id, request["method"], request.get("params", {}))
        # JSON-RPC notifications have no responses, including method errors.
        return response if "id" in request else None

    def _dispatch(self, request_id: object, method: str, params: object) -> dict:
        def fail(number: int, code: str, message: str) -> dict:
            return error(request_id, number, code, message)

        def success(result: dict) -> dict:
            return {"jsonrpc": "2.0", "id": request_id, "result": result}

        known = {"initialize", "health.ping", "shutdown", "jobs.run", "jobs.cancel"}
        if self.allow_model_probe:
            known.add("diagnostics.model_probe")
        if method not in known:
            return fail(-32601, "METHOD_NOT_FOUND", "Method is not supported.")
        if method == "initialize":
            if self.configuration is not None:
                return fail(-32003, "ALREADY_INITIALIZED", "Connection is already initialized.")
            if not isinstance(params, dict):
                return fail(-32602, "INVALID_PARAMS", "Initialize requires named parameters.")
            required = {"protocol_version", "instance_id", "job_root", "profile_id", "profile_version"}
            if set(params) != required or type(params["protocol_version"]) is not int:
                return fail(-32602, "INVALID_PARAMS", "Invalid initialize fields.")
            if params["protocol_version"] != PROTOCOL_VERSION:
                return fail(-32001, "UNSUPPORTED_PROTOCOL", "Only protocol version 1 is supported.")
            try:
                if not isinstance(params["instance_id"], str):
                    raise ValueError
                UUID(params["instance_id"])
                if not isinstance(params["job_root"], str):
                    raise ValueError
                root = Path(params["job_root"])
                if not root.is_absolute() or not root.is_dir():
                    raise ValueError
                for field in ("profile_id", "profile_version"):
                    if not isinstance(params[field], str) or not params[field].strip() or len(params[field]) > 128:
                        raise ValueError
            except (ValueError, OSError):
                return fail(-32602, "INVALID_PARAMS", "Invalid host identity, existing job directory, or profile.")
            self.configuration = dict(params)
            return success({
                "protocol_version": PROTOCOL_VERSION,
                "build_version": __version__,
                "instance_id": params["instance_id"],
                "capabilities": {"health": True, "task_types": [], "model_clients": []},
            })

        if self.configuration is None:
            return fail(-32002, "NOT_INITIALIZED", "Initialize the connection first.")
        if method in {"jobs.run", "jobs.cancel"}:
            return fail(-32601, "CAPABILITY_UNAVAILABLE", "Analysis jobs are not implemented in this build.")
        if method == "diagnostics.model_probe":
            # Imports are lazy: normal lifecycle connections never import MLX.
            from .model_probe import ProbeError, model_probe

            try:
                return success(model_probe(params))
            except ProbeError as exc:
                return fail(exc.number, exc.code, str(exc))
        if not isinstance(params, dict) or params:
            return fail(-32602, "INVALID_PARAMS", "This method accepts no parameters.")
        if method == "health.ping":
            return success({
                "status": "ok",
                "protocol_version": PROTOCOL_VERSION,
                "instance_id": self.configuration["instance_id"],
            })
        self.stopping = True
        return success({"status": "shutting_down"})
