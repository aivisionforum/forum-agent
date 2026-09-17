"""Exercise refusal/verification before any package or model download."""

from __future__ import annotations

import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[3] / "desktop/scripts/package_meeting_worker.py"
spec = importlib.util.spec_from_file_location("worker_packager", SCRIPT)
packager = importlib.util.module_from_spec(spec)
spec.loader.exec_module(packager)


class PackagingBoundaryTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()

    def test_output_refuses_existing_root_relative_parent_and_symlink(self):
        outside = self.root / "existing"
        outside.mkdir()
        sentinel = outside / "keep.txt"
        sentinel.write_text("must survive")
        link = self.root / "link"
        link.symlink_to(outside, target_is_directory=True)
        for path in (Path("/"), Path("relative"), outside, link, self.root / ".." / "escape"):
            with self.subTest(path=path), self.assertRaises(ValueError):
                packager.new_output(path)
        self.assertEqual(packager.new_output(self.root / "new"), self.root / "new")
        self.assertFalse((self.root / "new").exists())
        self.assertEqual(sentinel.read_text(), "must survive")

    def test_offline_cache_verifies_hash_and_rejects_symlink_without_changes(self):
        content = b"synthetic wheel contents"
        artifact = {"filename": "fake.whl", "url": "https://files.pythonhosted.org/fake.whl",
                    "sha256": hashlib.sha256(content).hexdigest(), "size": len(content)}
        target = self.root / "fake.whl"
        target.write_bytes(content)
        self.assertEqual(packager.fetch(artifact, self.root, offline=True), target)
        target.write_bytes(b"changed")
        with self.assertRaises(ValueError):
            packager.fetch(artifact, self.root, offline=True)
        self.assertEqual(target.read_bytes(), b"changed")
        target.unlink()
        with self.assertRaises(ValueError):
            packager.fetch(artifact, self.root, offline=True)
        outside = self.root / "keep.whl"
        outside.write_bytes(content)
        target.symlink_to(outside)
        with self.assertRaises(ValueError):
            packager.fetch(artifact, self.root, offline=True)
        self.assertEqual(outside.read_bytes(), content)

    def test_checked_in_lock_has_official_hash_pins_and_macos14_wheels(self):
        self._check_lock(packager.LOCK)
        self._check_lock(packager.REPO / "services/asr-worker/runtime-macos-arm64.lock.json")

    def test_installed_prompt_manifest_matches_six_source_resources(self):
        directory = self.root / packager.PROMPT_DIRECTORY
        directory.mkdir(parents=True)
        source_hashes = {}
        for kind in packager.ANALYSIS_TASK_TYPES:
            path = directory / f'{kind}.txt'
            content = (packager.WORKER / f'src/forum_meeting_worker/prompts/v1/{kind}.txt').read_bytes()
            path.write_bytes(content)
            source_hashes[f'src/forum_meeting_worker/prompts/v1/{kind}.txt'] = hashlib.sha256(content).hexdigest()
        actual = packager.analysis_prompt_manifest(self.root, source_hashes)
        self.assertEqual(set(actual), set(packager.ANALYSIS_TASK_TYPES))
        for kind, record in actual.items():
            self.assertEqual(record['prompt_version'], kind + '-v1')
            self.assertEqual(record['sha256'], source_hashes[f'src/forum_meeting_worker/prompts/v1/{kind}.txt'])
        path = directory / 'minutes.txt'
        before = path.read_bytes()
        path.write_bytes(before + b'changed after source copy')
        with self.assertRaisesRegex(ValueError, 'differs from the packaged source'):
            packager.analysis_prompt_manifest(self.root, source_hashes)
        path.unlink()
        with self.assertRaises(ValueError):
            packager.analysis_prompt_manifest(self.root, source_hashes)
        path.symlink_to(directory / 'insight.txt')
        with self.assertRaisesRegex(ValueError, 'regular files'):
            packager.analysis_prompt_manifest(self.root, source_hashes)

    def _check_lock(self, path):
        lock = json.loads(path.read_text())
        for artifact in [lock["python"], *lock["wheels"]]:
            packager.validate_artifact(artifact)
        self.assertEqual(lock["python"]["version"], "3.12.11")
        self.assertEqual(lock["minimum_macos"], "14.0")
        for artifact in lock["wheels"]:
            self.assertNotIn("macosx_26_", artifact["filename"])
            self.assertNotIn("macosx_15_", artifact["filename"])


if __name__ == "__main__":
    unittest.main()
