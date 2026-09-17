"""Release gates must never label an ad-hoc or mismatched app as Developer ID."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT_DIR = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('forum_release_check', SCRIPT_DIR / 'check_forum_app.py')
checker = importlib.util.module_from_spec(spec); spec.loader.exec_module(checker)
spec = importlib.util.spec_from_file_location('forum_native_inventory', SCRIPT_DIR / 'list_forum_native_code.py')
inventory = importlib.util.module_from_spec(spec); spec.loader.exec_module(inventory)


class ReleaseGateTests(unittest.TestCase):
    def test_signing_inventory_recognizes_native_variants_and_metal(self):
        for magic in inventory.MACH_MAGICS:
            self.assertEqual(inventory.code_kind(magic), 'Mach-O')
        for magic in inventory.FAT_MAGICS:
            self.assertEqual(inventory.code_kind(magic), 'FAT')
        self.assertEqual(inventory.code_kind(b'MTLB'), 'MetalLib')
        for magic in [b'#!py', b'import', b'', b'PK\x03\x04']:
            self.assertIsNone(inventory.code_kind(magic))

    def test_signing_inventory_defers_main_and_never_follows_links(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); app = root / 'Forum.app'; macos = app / 'Contents/MacOS'
            macos.mkdir(parents=True)
            (macos / 'forum-shell').write_bytes(b'\xcf\xfa\xed\xfeignored until final app signing')
            native = macos / 'sidecar'; native.write_bytes(b'\xcf\xfa\xed\xfe')
            metal = macos / 'mlx.metallib'; metal.write_bytes(b'MTLB')
            (macos / 'module.py').write_text('pass')
            outside = root / 'outside'; outside.mkdir(); (outside / 'foreign').write_bytes(b'MTLB')
            (macos / 'foreign').symlink_to(outside / 'foreign')
            (macos / 'foreign-directory').symlink_to(outside, target_is_directory=True)
            self.assertEqual(inventory.native_paths(app), sorted([native.resolve(), metal.resolve()]))

    def test_developer_id_requires_certificate_chain_team_and_hardened_runtime(self):
        real = 'CodeDirectory v=20500 size=123 flags=0x10000(runtime) hashes=42+7\nAuthority=Developer ID Application: Example (ABCDE12345)\nAuthority=Developer ID Certification Authority\nTeamIdentifier=ABCDE12345\n'
        self.assertEqual(checker.signature_kind(real), 'developer_id_application')
        self.assertEqual(checker.signature_kind(real + 'Signature=adhoc\n'), 'ad_hoc_development')
        for changed in [real.replace('Developer ID Application:', 'Apple Development:'),
                        real.replace('TeamIdentifier=ABCDE12345', 'TeamIdentifier=not set'),
                        real.replace('0x10000(runtime)', '0x0(none)')]:
            self.assertEqual(checker.signature_kind(changed), 'unrecognized')

    def test_manifest_cannot_convert_dev_to_release_or_claim_notarization(self):
        manifest = {'owner': checker.OWNER, 'schema_version': 1, 'profile': 'dev',
            'model_weights_included': False, 'signing_policy': 'ad_hoc_development',
            'notarization': 'not_submitted', 'input_sha256': {name: '0' * 64 for name in checker.INPUTS}}
        checker.validate_manifest(manifest)
        for changed in [dict(manifest, profile='release'), dict(manifest, notarization='accepted'), dict(manifest, model_weights_included=True)]:
            with self.assertRaises(ValueError):
                checker.validate_manifest(changed)

    def test_runtime_source_hash_rejects_stale_and_modified_module(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            file = root / checker.RUNTIME_SITE / 'forum_speaker_worker/main.py'
            file.parent.mkdir(parents=True); file.write_text('original')
            manifest = {'source_sha256': {'src/forum_speaker_worker/main.py': checker.sha(file)}}
            checker.check_runtime_sources(root, manifest)
            file.write_text('modified')
            with self.assertRaisesRegex(ValueError, 'differs'):
                checker.check_runtime_sources(root, manifest)

    def test_release_missing_certificate_refuses_before_build_or_file_changes(self):
        environment = {key: value for key, value in os.environ.items() if key != 'APPLE_SIGNING_IDENTITY'}
        for script, arguments in [('build_macos_app.sh', ['--profile', 'release']),
                                  ('macos_sign_and_notarize.sh', ['--app', '/nonexistent/app.app', '--profile', 'release'])]:
            result = subprocess.run(['/bin/bash', str(SCRIPT_DIR / script), *arguments], env=environment,
                text=True, capture_output=True, timeout=5)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('Developer ID', result.stderr)


if __name__ == '__main__':
    unittest.main()
