"""Exercise actual sidecar pipes; no model, web service, or fixture recording."""

from __future__ import annotations

import json
import os
from pathlib import Path
import selectors
import subprocess
import sys
import tempfile
import unittest

PACKAGE_ROOT = Path(__file__).resolve().parents[1]
MAX_FRAME_BYTES = 1024 * 1024


def request(method: str, request_id: object = 1, **params: object) -> dict:
    return {"jsonrpc": "2.0", "id": request_id, "method": method, "params": params}


def encode(*messages: object) -> bytes:
    return b"".join(json.dumps(message).encode() + b"\n" for message in messages)


class WorkerProtocolTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.env = os.environ.copy()
        self.env["PYTHONPATH"] = str(PACKAGE_ROOT / "src")
        self.command = [sys.executable, "-m", "forum_meeting_worker"]
        self.init = request(
            "initialize", "init",
            protocol_version=1,
            instance_id="90000000-0000-4000-8000-000000000001",
            job_root=self.directory.name,
            profile_id="ai-vision-forum",
            profile_version="v1",
        )

    def run_worker(self, data: bytes, *args: str) -> tuple[subprocess.CompletedProcess, list[dict]]:
        result = subprocess.run(
            self.command + list(args), input=data, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, env=self.env, cwd=self.directory.name, timeout=5,
        )
        messages = [json.loads(line) for line in result.stdout.splitlines()]
        return result, messages

    def test_initialize_health_unavailable_job_and_shutdown(self) -> None:
        result, messages = self.run_worker(encode(
            self.init, request("health.ping", 2),
            request("jobs.run", 3, kind="insights"), request("jobs.cancel", 4),
            request("shutdown", 5), request("health.ping", "after-shutdown"),
        ))
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        self.assertEqual([message["id"] for message in messages], ["init", 2, 3, 4, 5])
        initialized = messages[0]["result"]
        self.assertEqual(initialized["protocol_version"], 1)
        self.assertEqual(initialized["build_version"], "0.1.0")
        self.assertEqual(initialized["capabilities"], {
            "health": True, "task_types": [], "model_clients": [],
        })
        self.assertEqual(messages[1]["result"]["status"], "ok")
        self.assertEqual(messages[2]["error"]["data"]["code"], "CAPABILITY_UNAVAILABLE")
        self.assertEqual(messages[3]["error"]["data"]["code"], "CAPABILITY_UNAVAILABLE")
        self.assertEqual(messages[4]["result"]["status"], "shutting_down")
        self.assertIn(b"analysis capabilities unavailable", result.stderr)
        self.assertEqual(list(Path(self.directory.name).iterdir()), [])

    def test_methods_require_initialize_and_failed_init_can_retry(self) -> None:
        unsupported = json.loads(json.dumps(self.init))
        unsupported["params"]["protocol_version"] = 2
        result, messages = self.run_worker(encode(
            request("health.ping"), request("jobs.run", 2), request("shutdown", 3),
            unsupported, self.init, request("health.ping", 6),
        ))
        self.assertEqual(result.returncode, 0)
        self.assertEqual([m["error"]["data"]["code"] for m in messages[:4]], [
            "NOT_INITIALIZED", "NOT_INITIALIZED", "NOT_INITIALIZED", "UNSUPPORTED_PROTOCOL",
        ])
        self.assertEqual(messages[-1]["result"]["status"], "ok")

    def test_duplicate_initialize_cannot_replace_host_identity(self) -> None:
        another = json.loads(json.dumps(self.init))
        another["params"]["instance_id"] = "90000000-0000-4000-8000-000000000002"
        result, messages = self.run_worker(encode(self.init, another, request("health.ping")))
        self.assertEqual(result.returncode, 0)
        self.assertEqual(messages[1]["error"]["data"]["code"], "ALREADY_INITIALIZED")
        self.assertEqual(messages[2]["result"]["instance_id"], self.init["params"]["instance_id"])

    def test_initialize_validates_host_fields_without_creating_paths(self) -> None:
        invalid_fields = (
            ("protocol_version", True), ("instance_id", "not-a-uuid"),
            ("job_root", "relative/jobs"),
            ("job_root", str(Path(self.directory.name) / "missing")),
            ("profile_id", ""), ("profile_version", None),
        )
        for field, value in invalid_fields:
            with self.subTest(field=field, value=value):
                invalid = json.loads(json.dumps(self.init))
                invalid["params"][field] = value
                result, messages = self.run_worker(encode(invalid, self.init))
                self.assertEqual(result.returncode, 0)
                self.assertEqual(messages[0]["error"]["code"], -32602)
                self.assertIn("result", messages[1])
        self.assertEqual(list(Path(self.directory.name).iterdir()), [])

    def test_request_validation_preserves_connection(self) -> None:
        invalid_requests = (
            [], [request("health.ping")], {"method": "health.ping", "id": 1},
            request("health.ping", True), {"jsonrpc": "2.0", "method": 12, "id": 1},
            {"jsonrpc": "2.0", "method": "health.ping", "params": None, "id": 1},
        )
        result, messages = self.run_worker(encode(
            self.init, *invalid_requests, request("health.ping", "still-alive"),
        ))
        self.assertEqual(result.returncode, 0)
        for message in messages[1:-1]:
            self.assertIsNone(message["id"])
            self.assertEqual(message["error"]["code"], -32600)
        self.assertEqual(messages[-1]["id"], "still-alive")
        self.assertIn("result", messages[-1])

    def test_parse_errors_do_not_echo_input_or_kill_the_connection(self) -> None:
        invalid_frames = (
            b'{broken private-meeting-text}\n', b'\xff\n',
            b'{"jsonrpc":"2.0","jsonrpc":"2.0","method":"health.ping","id":1}\n',
            b'{"jsonrpc":"2.0","method":"health.ping","id":NaN}\n',
        )
        result, messages = self.run_worker(encode(self.init) + b"".join(invalid_frames)
                                           + encode(request("health.ping", "alive")))
        self.assertEqual(result.returncode, 0)
        self.assertTrue(all(message["error"]["code"] == -32700 for message in messages[1:-1]))
        self.assertEqual(messages[-1]["id"], "alive")
        self.assertNotIn(b"private-meeting-text", result.stdout + result.stderr)

    def test_unknown_method_and_invalid_method_params(self) -> None:
        positional = {"jsonrpc": "2.0", "method": "health.ping", "params": [], "id": 3}
        result, messages = self.run_worker(encode(
            self.init, request("does.not.exist", 2), positional,
            request("health.ping", 4, unexpected=True), request("shutdown", 5),
        ))
        self.assertEqual(result.returncode, 0)
        self.assertEqual(messages[1]["error"]["code"], -32601)
        self.assertEqual(messages[2]["error"]["code"], -32602)
        self.assertEqual(messages[3]["error"]["code"], -32602)

    def test_notifications_do_not_emit_responses_and_shutdown_exits(self) -> None:
        notifications = [
            {"jsonrpc": "2.0", "method": "health.ping"},
            {"jsonrpc": "2.0", "method": "unknown.method"},
            {"jsonrpc": "2.0", "method": "jobs.run", "params": {}},
            {"jsonrpc": "2.0", "method": "shutdown"},
        ]
        result, messages = self.run_worker(encode(self.init, *notifications, request("health.ping")))
        self.assertEqual(result.returncode, 0)
        self.assertEqual(len(messages), 1)

    def test_numeric_null_and_unicode_ids_round_trip(self) -> None:
        ids = [None, 0, 3.5, 10 ** 400, "引用📝"]
        result, messages = self.run_worker(encode(self.init, *(request("health.ping", i) for i in ids)))
        self.assertEqual(result.returncode, 0)
        self.assertEqual([m["id"] for m in messages[1:]], ids)

    def test_frame_limit_exact_boundary_and_oversized_frame(self) -> None:
        ping = json.dumps(request("health.ping", 2)).encode()
        exact = ping + b" " * (MAX_FRAME_BYTES - len(ping)) + b"\n"
        result, messages = self.run_worker(encode(self.init) + exact)
        self.assertEqual(result.returncode, 0)
        self.assertIn("result", messages[-1])
        oversized = b" " * (MAX_FRAME_BYTES + 1) + b"\n"
        result, messages = self.run_worker(encode(self.init) + oversized + encode(request("health.ping")))
        self.assertEqual(result.returncode, 2)
        self.assertEqual(len(messages), 2)
        self.assertEqual(messages[-1]["error"]["data"]["code"], "FRAME_TOO_LARGE")

    def test_eof_with_partial_frame_is_not_executed(self) -> None:
        result, messages = self.run_worker(encode(self.init) + json.dumps(request("shutdown")).encode())
        self.assertEqual(result.returncode, 2)
        self.assertEqual(messages[-1]["error"]["data"]["code"], "INCOMPLETE_FRAME")

    def test_incomplete_frame_deadline_ends_a_stalled_pipe(self) -> None:
        with subprocess.Popen(
            self.command + ["--frame-timeout-seconds", "0.1"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            env=self.env, cwd=self.directory.name,
        ) as process:
            try:
                process.stdin.write(b'{"jsonrpc":')
                process.stdin.flush()
                with selectors.DefaultSelector() as selector:
                    selector.register(process.stdout, selectors.EVENT_READ)
                    self.assertTrue(selector.select(3), "worker did not enforce frame deadline")
                response = json.loads(process.stdout.readline())
                self.assertEqual(response["error"]["data"]["code"], "FRAME_TIMEOUT")
                self.assertEqual(process.wait(timeout=3), 2)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=3)

    def test_clean_eof_needs_no_initialize(self) -> None:
        result, messages = self.run_worker(b"")
        self.assertEqual(result.returncode, 0)
        self.assertEqual(messages, [])

    def test_invalid_frame_timeout_is_rejected_on_stderr(self) -> None:
        result, messages = self.run_worker(b"", "--frame-timeout-seconds", "nan")
        self.assertEqual(result.returncode, 2)
        self.assertEqual(messages, [])
        self.assertIn(b"finite positive number", result.stderr)


if __name__ == "__main__":
    unittest.main()
