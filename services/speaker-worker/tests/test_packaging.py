from __future__ import annotations
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

REPO = Path(__file__).resolve().parents[3]
WORKER = REPO / 'services/speaker-worker'


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
    return module


packager = load('speaker_package_test', REPO / 'desktop/scripts/package_speaker_worker.py')
checker = load('speaker_check_test', WORKER / 'packaging/check_bundle.py')


class PackagingTests(unittest.TestCase):
    def test_check_timeout_default_override_and_invalid_values(self):
        with patch.dict(checker.os.environ, {}, clear=True):
            self.assertEqual(checker.check_timeout(), 300)
        with patch.dict(checker.os.environ, {checker.TIMEOUT_ENV: '600'}):
            self.assertEqual(checker.check_timeout(), 600)
        for value in ('', 'invalid', '0', '-1', 'nan', 'inf'):
            with self.subTest(value=value), patch.dict(checker.os.environ, {checker.TIMEOUT_ENV: value}):
                with self.assertRaisesRegex(ValueError, checker.TIMEOUT_ENV):
                    checker.check_timeout()

    def test_timeout_override_reaches_sanitized_packaging_check(self):
        with patch.dict(packager.os.environ, {checker.TIMEOUT_ENV: '600', 'PRIVATE_TOKEN': 'secret'}):
            env = packager.checker_environment({'PATH': '/usr/bin'})
        self.assertEqual(env, {'PATH': '/usr/bin', checker.TIMEOUT_ENV: '600'})

    def test_lock_pins_official_artifacts_and_separate_runtime(self):
        lock = json.loads((WORKER / 'packaging/runtime-macos-arm64.lock.json').read_text())
        packager.validate_lock(lock)
        self.assertFalse(lock['model_weights_included'])
        self.assertEqual(lock['python']['version'], '3.12.11')
        names = {wheel['name'].lower() for wheel in lock['wheels'] if wheel['role'] == 'runtime'}
        self.assertIn('speechbrain', names)
        self.assertNotIn('mlx-lm', names)
        self.assertNotIn('mlx-whisper', names)

    def test_lock_refuses_download_redirect_identity_and_duplicates(self):
        for mutation in ('url', 'sha', 'duplicate', 'weights', 'filename'):
            lock = json.loads((WORKER / 'packaging/runtime-macos-arm64.lock.json').read_text())
            wheel = next(w for w in lock['wheels'] if w['role'] == 'runtime')
            if mutation == 'url':
                wheel['url'] = 'https://example.invalid/dependency.whl'
            elif mutation == 'sha':
                wheel['sha256'] = '0'
            elif mutation == 'duplicate':
                lock['wheels'].append(wheel.copy())
            elif mutation == 'weights':
                lock['model_weights_included'] = True
            else:
                wheel['filename'] = '../outside.whl'
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                packager.validate_lock(lock)

    def test_existing_output_is_never_removed_or_overwritten(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sentinel = root / 'important'; sentinel.write_text('keep')
            with self.assertRaises(ValueError):
                packager.package(root, root / 'cache', offline=True)
            self.assertEqual(sentinel.read_text(), 'keep')
            self.assertFalse((root / 'cache').exists())

    def test_bundle_checks_installed_source_and_model_exclusion(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / checker.SITE / 'forum_speaker_worker/__init__.py'
            path.parent.mkdir(parents=True); path.write_text('VERSION = 1\n')
            scripts = root / 'checks'; scripts.mkdir()
            for name in ('check_bundle.py', '_native_check.py'):
                (scripts / name).write_text('# checker fixture\n')
            licenses = root / 'third-party/python'; licenses.mkdir(parents=True)
            (licenses / 'PYTHON.json').write_text('{}')
            (licenses / 'LICENSE.python.txt').write_text('license fixture')
            manifest = {'owner': checker.OWNER, 'schema_version': 1, 'target': 'aarch64-apple-darwin',
                'task_types': ['speaker_embedding'], 'model_weights_included': False, 'formal_product_acceptance': 'not_evaluated',
                'entrypoint': 'bin/speaker-worker', 'source_sha256': {'src/forum_speaker_worker/__init__.py': checker.digest(path)},
                'check_script_sha256': checker.digest(scripts / 'check_bundle.py'), 'native_check_script_sha256': checker.digest(scripts / '_native_check.py'),
                'python_license_files': ['LICENSE.python.txt']}
            (root / 'runtime-manifest.json').write_text(json.dumps(manifest))
            checker.check_paths(root)
            path.write_text('VERSION = 2\n')
            with self.assertRaisesRegex(ValueError, 'digest mismatch'):
                checker.check_paths(root)
            path.write_text('VERSION = 1\n')
            (root / 'embedding_model.ckpt').write_bytes(b'not allowed')
            with self.assertRaisesRegex(ValueError, 'weights'):
                checker.check_paths(root)

    def test_offline_missing_artifact_does_not_fetch(self):
        lock = json.loads((WORKER / 'packaging/runtime-macos-arm64.lock.json').read_text())
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(ValueError, 'Missing offline artifact'):
                packager.base.fetch(lock['python'], Path(directory), True)


if __name__ == '__main__':
    unittest.main()
