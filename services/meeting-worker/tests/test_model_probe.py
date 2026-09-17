"""Probe boundaries only: these tests never import MLX or load real weights."""

from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

PACKAGE_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(PACKAGE_ROOT / "src"))
from forum_meeting_worker.model_probe import ProbeError, validate_probe
from forum_meeting_worker.protocol import Protocol


class ModelProbeBoundaryTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / "config.json").write_text('{"model_type":"qwen3"}')
        (self.root / "tokenizer_config.json").write_text('{}')
        (self.root / "tokenizer.json").write_text('{}')
        (self.root / "model.safetensors").write_bytes(b"synthetic nonempty weights; not a real model")
        self.params = {"model_path": str(self.root), "prompt": "合成诊断", "max_tokens": 16}

    def test_local_input_bounds_without_importing_model(self):
        for field, value in (("model_path", "mlx-community/Qwen3-8B-4bit"),
                             ("model_path", str(self.root / "missing")),
                             ("max_tokens", True), ("max_tokens", 129), ("max_tokens", 0),
                             ("prompt", ""), ("prompt", "x" * 4097)):
            with self.subTest(field=field, value=value), self.assertRaises(ProbeError):
                validate_probe({**self.params, field: value})
        self.assertEqual(validate_probe(self.params), (self.root.resolve(), "合成诊断", 16))
        self.assertNotIn("mlx.core", sys.modules)

    def test_rejects_custom_code_and_incomplete_weights(self):
        for config in ({"model_type": "qwen3", "model_file": "custom.py"},
                       {"model_type": "qwen3", "auto_map": {}}, {"model_type": "other"}):
            (self.root / "config.json").write_text(json.dumps(config))
            with self.assertRaises(ProbeError):
                validate_probe(self.params)
        (self.root / "config.json").write_text('{"model_type":"qwen3"}')
        (self.root / "model.safetensors").unlink()
        (self.root / "model.safetensors.index.json").write_text('{"weight_map":{"x":"../outside.safetensors"}}')
        with self.assertRaises(ProbeError):
            validate_probe(self.params)
        (self.root / "model.safetensors.index.json").write_text('{"weight_map":{"x":"model-1.safetensors"}}')
        with self.assertRaises(ProbeError):
            validate_probe(self.params)
        (self.root / "model-1.safetensors").write_bytes(b"synthetic")
        self.assertEqual(validate_probe(self.params)[2], 16)

    def test_probe_opt_in_does_not_grant_formal_jobs(self):
        init = {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocol_version": 1, "instance_id": "90000000-0000-4000-8000-000000000001",
            "job_root": str(self.root), "profile_id": "forum", "profile_version": "v1",
        }}
        for enabled in (False, True):
            protocol = Protocol(allow_model_probe=enabled)
            reply = protocol.handle(json.dumps(init).encode())
            self.assertIn("minutes", reply["result"]["capabilities"]["task_types"])
            self.assertNotIn("model_grants", protocol.configuration)
            for method, expected in (("jobs.run", "INVALID_PARAMS"),
                                     ("diagnostics.model_probe", "INVALID_PARAMS" if enabled else "METHOD_NOT_FOUND")):
                reply = protocol.handle(json.dumps({"jsonrpc": "2.0", "id": 2, "method": method, "params": {}}).encode())
                self.assertEqual(reply["error"]["data"]["code"], expected)
        self.assertNotIn("mlx.core", sys.modules)

    def test_native_stdout_is_redirected_and_restored(self):
        env = {"PATH": "/usr/bin:/bin", "PYTHONPATH": str(PACKAGE_ROOT / "src")}
        code = ("from forum_meeting_worker.model_probe import library_stdout_to_stderr\n"
                "import os, ctypes\nwith library_stdout_to_stderr():\n"
                " print('python library chatter')\n os.write(1,b'native library chatter\\n')\n"
                " ctypes.CDLL(None).printf(b'buffered C library chatter\\n')\n"
                "os.write(1,b'{\"jsonrpc\":\"2.0\"}\\n')\n")
        result = subprocess.run([sys.executable, "-c", code], env=env, cwd=self.root,
                                capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, b'{"jsonrpc":"2.0"}\n')
        self.assertIn(b"native library chatter", result.stderr)
        self.assertIn(b"python library chatter", result.stderr)
        self.assertIn(b"buffered C library chatter", result.stderr)


if __name__ == "__main__":
    unittest.main()
