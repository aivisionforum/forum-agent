import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
from uuid import uuid4

spec = importlib.util.spec_from_file_location("asr_worker", Path(__file__).parents[1] / "asr_worker.py")
worker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(worker)


class AdapterTests(unittest.TestCase):
    def request(self, **overrides):
        return {"id": str(uuid4()), "samples": [0.0, 0.1], "sample_rate": 16000,
                "language": "auto", **overrides}

    def test_detected_language_comes_from_model_and_failure_is_not_empty(self):
        class Engine:
            def transcribe(self, samples, language):
                self.input = language
                return "API 还没通过", "zh"
        engine = Engine()
        result = worker.response(engine, self.request())
        self.assertEqual(engine.input, "auto")
        self.assertEqual(result["detected_language"], "zh")
        self.assertEqual(result["status"], "success")
        class Failed:
            def transcribe(self, *args):
                raise RuntimeError("private source text")
        result = worker.response(Failed(), self.request())
        self.assertEqual(result["status"], "failed")
        self.assertNotIn("private", str(result))

    def test_rejects_invalid_pcm_and_file_path_requests(self):
        for change in [dict(samples=[]), dict(samples=[float("nan")]),
                       dict(samples=[float("inf")]), dict(samples=[True]),
                       dict(samples=[1.1]), dict(samples="/tmp/audio.wav"),
                       dict(samples=[0] * (worker.MAX_SAMPLES + 1)),
                       dict(sample_rate=44100), dict(sample_rate=True),
                       dict(language="auto;run"), dict(id="0"), dict(path="any")]:
            with self.subTest(change=list(change)):
                with self.assertRaises(worker.ProtocolError):
                    worker.validate_request(self.request(**change))

    def test_local_model_does_not_accept_repo_name_or_npz_fallback(self):
        with self.assertRaises(ValueError):
            worker.local_model(Path("mlx-community/whisper"))
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "config.json").write_text("{}")
            (root / "weights.npz").write_bytes(b"untrusted")
            with self.assertRaises(ValueError):
                worker.local_model(root)
            (root / "weights.safetensors").write_bytes(b"test")
            self.assertEqual(worker.local_model(root), root.resolve())

    def test_frame_reader_preserves_next_frame_and_detects_truncated_input(self):
        read_fd, write_fd = os.pipe()
        with os.fdopen(read_fd, "rb", buffering=0) as stream:
            os.write(write_fd, b'{"a":1}\n{"b":2}\n{')
            os.close(write_fd)
            reader = worker.FrameReader(stream)
            self.assertEqual(reader.read(), {"a": 1})
            self.assertEqual(reader.read(), {"b": 2})
            with self.assertRaisesRegex(worker.ProtocolError, "TRUNCATED_FRAME"):
                reader.read()

    def test_partial_frame_has_absolute_deadline(self):
        read_fd, write_fd = os.pipe()
        with os.fdopen(read_fd, "rb", buffering=0) as stream:
            try:
                os.write(write_fd, b'{')
                with self.assertRaisesRegex(worker.ProtocolError, "FRAME_TIMEOUT"):
                    worker.FrameReader(stream).read(timeout=0.02)
            finally:
                os.close(write_fd)

    def test_exact_digital_silence_does_not_hallucinate(self):
        engine = worker.WhisperEngine.__new__(worker.WhisperEngine)
        self.assertEqual(engine.transcribe([0.0] * 1600, "auto"), ("", None))


if __name__ == "__main__":
    unittest.main()
