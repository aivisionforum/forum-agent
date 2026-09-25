"""Validate the generated Forum schema without importing the server or MLX.

The caller supplies the exact bundled forum.schema.json. This module is a
structural boundary plus UTF-8 proof helper; SQLite remains authoritative for
session ownership, current revisions, direction epochs, and attempt fences.
No worker job capability is enabled by this module.
"""
from __future__ import annotations

import json
import math
from pathlib import Path
from typing import Any
from uuid import UUID

SCHEMA_VERSION = 1
MAX_SAFE_INTEGER = 9_007_199_254_740_991


class ContractError(ValueError):
    pass


class ContractValidator:
    def __init__(self, schema_path: str | Path):
        path = Path(schema_path)
        if path.stat().st_size > 2 * 1024 * 1024:
            raise ContractError("schema exceeds 2 MiB")
        self.schema = json.loads(path.read_text(encoding="utf-8"))
        if (self.schema.get("schema_version") != SCHEMA_VERSION
                or self.schema.get("$id") != "urn:ai-vision-forum:contracts:1"):
            raise ContractError("unsupported contract schema")

    def validate(self, value: Any, definition: str = "ForumEvent") -> None:
        schema = self.schema.get("$defs", {}).get(definition)
        if schema is None:
            raise ContractError("unknown contract definition")
        self._check(value, schema, "$", 0)

    def validate_event(self, value: Any, *, allow_host_recovery: bool = False) -> None:
        self.validate(value)
        if value["type"] == "session.producer_reconciled" and not allow_host_recovery:
            raise ContractError("producer reconciliation requires trusted host evidence")
        payload = value["payload"]
        if "audio" in payload:
            audio = payload["audio"]
            if (audio["end_sample"] <= audio["start_sample"]
                    or audio["end_ms"] < audio["start_ms"]):
                raise ContractError("invalid audio range")
        if "source_spans" in payload:
            separators = {"identity-v1": "", "join-space-v1": " ", "join-newline-v1": "\n"}
            separator = separators.get(payload["normalization_version"])
            if separator is None:
                raise ContractError("unsupported normalization")
            spans = payload["source_spans"]
            context = payload.get("context_spans", [])
            if len(context) > 4 or sum(len(s["quote"]) for s in context) > 720:
                raise ContractError("translation context budget exceeded")
            all_spans = spans + context
            for i, span in enumerate(all_spans):
                if (span["end_utf8"] <= span["start_utf8"]
                        or len(span["quote"].encode("utf-8")) != span["end_utf8"] - span["start_utf8"]):
                    raise ContractError("invalid UTF-8 source range")
                for prior in all_spans[:i]:
                    if (prior["segment_id"] == span["segment_id"]
                            and prior["segment_revision"] == span["segment_revision"]
                            and prior["start_utf8"] < span["end_utf8"]
                            and span["start_utf8"] < prior["end_utf8"]):
                        raise ContractError("overlapping source ranges")
            if not payload["input_text"] or separator.join(s["quote"] for s in spans) != payload["input_text"]:
                raise ContractError("input text does not reconstruct from exact sources")
        if value["type"] == "transcript.final":
            if payload["status"] == "success":
                if not payload["text"].strip():
                    raise ContractError("successful transcript must have text")
            elif payload["text"] or not (payload.get("reason") or "").strip():
                raise ContractError("non-success transcript needs empty text and reason")
        if "target_languages" in payload:
            languages = payload["target_languages"]
            if len(set(languages)) != len(languages) or any(not s.strip() for s in languages):
                raise ContractError("target languages must be unique and nonempty")

    def _check(self, value: Any, schema: Any, path: str, depth: int) -> None:
        if depth > 64:
            raise ContractError(f"{path}: nesting limit")
        if schema is True:
            return
        if schema is False:
            raise ContractError(f"{path}: disallowed")
        if "$ref" in schema:
            prefix = "#/$defs/"
            ref = schema["$ref"]
            if not ref.startswith(prefix) or ref[len(prefix):] not in self.schema["$defs"]:
                raise ContractError("nonlocal or unknown schema reference")
            self._check(value, self.schema["$defs"][ref[len(prefix):]], path, depth + 1)
        if "not" in schema:
            try:
                self._check(value, schema["not"], path, depth + 1)
            except ContractError:
                pass
            else:
                raise ContractError(f"{path}: excluded value")
        for key in ("oneOf", "anyOf"):
            if key in schema:
                matches = 0
                for branch in schema[key]:
                    try:
                        self._check(value, branch, path, depth + 1)
                        matches += 1
                    except ContractError:
                        pass
                if not matches or (key == "oneOf" and matches != 1):
                    raise ContractError(f"{path}: {key} mismatch")
        if "const" in schema and (value != schema["const"] or type(value) is not type(schema["const"])):
            raise ContractError(f"{path}: wrong constant")
        if "enum" in schema and value not in schema["enum"]:
            raise ContractError(f"{path}: unknown enum")
        if "type" in schema:
            expected = schema["type"]
            types = expected if isinstance(expected, list) else [expected]
            checks = {"object": isinstance(value, dict), "array": isinstance(value, list),
                      "string": isinstance(value, str), "integer": type(value) is int,
                      "number": type(value) in (int, float) and abs(value) <= MAX_SAFE_INTEGER and math.isfinite(value),
                      "boolean": type(value) is bool, "null": value is None}
            if not any(checks.get(kind, False) for kind in types):
                raise ContractError(f"{path}: expected {expected}")
        if type(value) in (int, float):
            if abs(value) > MAX_SAFE_INTEGER or not math.isfinite(value):
                raise ContractError(f"{path}: unsafe JSON number")
            if value < schema.get("minimum", -math.inf) or value > schema.get("maximum", math.inf):
                raise ContractError(f"{path}: numeric bounds")
        if isinstance(value, str):
            if schema.get("format") == "uuid":
                try:
                    parsed = UUID(value)
                    if not parsed.int or str(parsed) != value.lower():
                        raise ValueError("noncanonical UUID")
                except ValueError as error:
                    raise ContractError(f"{path}: invalid UUID") from error
            if len(value) < schema.get("minLength", 0):
                raise ContractError(f"{path}: string length")
        if isinstance(value, dict):
            if any(key not in value for key in schema.get("required", [])):
                raise ContractError(f"{path}: missing required field")
            props = schema.get("properties", {})
            for key, item in value.items():
                if key in props:
                    self._check(item, props[key], f"{path}.{key}", depth + 1)
                elif schema.get("additionalProperties") is False:
                    raise ContractError(f"{path}: unknown field {key}")
                elif isinstance(schema.get("additionalProperties"), dict):
                    self._check(item, schema["additionalProperties"], f"{path}.{key}", depth + 1)
        if isinstance(value, list):
            if len(value) < schema.get("minItems", 0):
                raise ContractError(f"{path}: too few items")
            if "items" in schema:
                for index, item in enumerate(value):
                    self._check(item, schema["items"], f"{path}[{index}]", depth + 1)


def validate_source_span(span: dict[str, Any], text: str) -> str:
    start, end = span["start_utf8"], span["end_utf8"]
    if type(start) is not int or type(end) is not int or not 0 <= start < end <= len(text.encode("utf-8")):
        raise ContractError("source span is empty or out of bounds")
    try:
        quote = text.encode("utf-8")[start:end].decode("utf-8", errors="strict")
    except UnicodeDecodeError as error:
        raise ContractError("source span splits a UTF-8 code point") from error
    if quote != span["quote"]:
        raise ContractError("source quote differs from stored revision")
    return quote
