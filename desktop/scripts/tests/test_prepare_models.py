"""Synthetic files only: never install into this machine's real model directory."""
import fcntl
import hashlib
import importlib.util
import json
import os
import shutil
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[2] / 'resources/setup/prepare_forum_models.py'
spec = importlib.util.spec_from_file_location('forum_model_preparation', SCRIPT)
prepare = importlib.util.module_from_spec(spec); spec.loader.exec_module(prepare)


def tensor_file(path, names=('fixture.weight',)):
    # MLX Whisper represents absent metadata as null in its existing snapshot.
    offsets, data, header = 0, bytearray(), {'__metadata__': None}
    for name in names:
        header[name] = {'dtype': 'F32', 'shape': [1], 'data_offsets': [offsets, offsets + 4]}
        offsets += 4; data.extend(struct.pack('<f', 0.5))
    encoded = json.dumps(header).encode()
    path.write_bytes(struct.pack('<Q', len(encoded)) + encoded + data)


class ModelPreparationTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(); self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()
        self.home = self.root / 'home'; self.home.mkdir()
        self.source = self.root / 'source'; self.source.mkdir()
        self.config = {'model_type': 'whisper', 'n_mels': 128, 'n_audio_state': 1280,
                       'n_audio_layer': 32, 'n_text_state': 1280, 'n_text_layer': 4}
        (self.source / 'config.json').write_text(json.dumps(self.config))
        tensor_file(self.source / 'weights.safetensors')
        self.models = self.home / 'Library/Application Support/AI Vision Forum/models'

    def test_copies_symlink_source_to_real_owned_files_and_records_sha(self):
        blob = self.root / 'blob'; (self.source / 'weights.safetensors').rename(blob)
        (self.source / 'weights.safetensors').symlink_to(blob)
        before = blob.stat()
        result = prepare.install('whisper', self.source, home=self.home)
        target = Path(result['path'])
        self.assertEqual(target, self.models / 'whisper-large-v3-turbo')
        self.assertFalse((target / 'weights.safetensors').is_symlink())
        receipt = json.loads((target / '.forum-model-install.json').read_text())
        self.assertEqual(receipt['files']['weights.safetensors']['sha256'], hashlib.sha256(blob.read_bytes()).hexdigest())
        self.assertEqual(before.st_mtime_ns, blob.stat().st_mtime_ns)
        self.assertFalse(receipt['inference_validated'])

    def test_existing_partial_complete_and_symlink_targets_are_never_replaced(self):
        self.models.mkdir(parents=True)
        target = self.models / 'whisper-large-v3-turbo'
        for mode in ('partial', 'complete', 'symlink'):
            if mode == 'symlink':
                target.symlink_to(self.source)
            else:
                target.mkdir(); (target / 'sentinel').write_text(mode)
            with self.subTest(mode=mode), self.assertRaisesRegex(prepare.PreparationError, 'already exists'):
                prepare.install('whisper', self.source, home=self.home)
            if target.is_symlink():
                target.unlink()
            else:
                self.assertEqual((target / 'sentinel').read_text(), mode)
                (target / 'sentinel').unlink(); target.rmdir()

    def test_destination_symlink_ancestor_rejected(self):
        outside = self.root / 'outside'; outside.mkdir()
        (self.home / 'Library').symlink_to(outside)
        with self.assertRaises(OSError):
            prepare.install('whisper', self.source, home=self.home)
        self.assertEqual(list(outside.iterdir()), [])

    def test_lock_rejects_concurrent_installer(self):
        self.models.mkdir(parents=True)
        with (self.models / '.model-install.lock').open('w') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            with self.assertRaisesRegex(prepare.PreparationError, 'Another Forum'):
                prepare.install('whisper', self.source, home=self.home)
        self.assertFalse((self.models / 'whisper-large-v3-turbo').exists())

    def test_hash_mismatch_does_not_publish_or_leave_stage(self):
        with self.assertRaisesRegex(prepare.PreparationError, 'fingerprint differs'):
            prepare.install('whisper', self.source, home=self.home, expected_manifest='sha256:' + '0' * 64)
        self.assertFalse((self.models / 'whisper-large-v3-turbo').exists())
        self.assertFalse(list(self.models.glob('.install-*')))

    def test_bad_architecture_and_tensor_bounds_rejected_before_install(self):
        self.config['n_text_layer'] = 32
        (self.source / 'config.json').write_text(json.dumps(self.config))
        with self.assertRaisesRegex(prepare.PreparationError, 'architecture'):
            prepare.install('whisper', self.source, home=self.home)
        self.assertFalse(self.models.exists())
        self.config['n_text_layer'] = 4
        (self.source / 'config.json').write_text(json.dumps(self.config))
        with (self.source / 'weights.safetensors').open('ab') as f:
            f.write(b'extra')
        with self.assertRaisesRegex(prepare.PreparationError, 'trailing'):
            prepare.install('whisper', self.source, home=self.home)

    def test_renaming_cannot_replace_concurrently_created_empty_target(self):
        self.models.mkdir(parents=True)
        (self.models / 'stage').mkdir(); (self.models / 'target').mkdir()
        fd = os.open(self.models, os.O_RDONLY | os.O_DIRECTORY)
        try:
            with self.assertRaises(FileExistsError):
                prepare.publish_exclusive(fd, 'stage', 'target')
        finally:
            os.close(fd)
        self.assertTrue((self.models / 'stage').is_dir())
        self.assertTrue((self.models / 'target').is_dir())

    def test_unapproved_ecapa_checkpoint_is_not_published(self):
        (self.source / 'embedding_model.ckpt').write_bytes(b'synthetic-not-real-ECAPA')
        with self.assertRaisesRegex(prepare.PreparationError, 'official model hash'):
            prepare.install('ecapa', self.source, home=self.home)
        self.assertFalse((self.models / 'ecapa').exists())

    def test_moved_model_root_cannot_report_false_install_success(self):
        original = prepare.inspect_model
        detached = self.models.with_name('detached-models')
        def moving_check(path, kind):
            if path.name.startswith('.install-'):
                self.models.rename(detached)
                self.models.mkdir()
                shutil.copytree(detached / path.name, self.models / path.name)
            return original(path, kind)
        with patch.object(prepare, 'inspect_model', side_effect=moving_check):
            with self.assertRaisesRegex(prepare.PreparationError, 'directory changed'):
                prepare.install('whisper', self.source, home=self.home)
        self.assertFalse((self.models / 'whisper-large-v3-turbo').exists())
        self.assertFalse((detached / 'whisper-large-v3-turbo').exists())
        self.assertFalse(list(detached.glob('.install-*')))

    def test_qwen_complete_index_and_receipt_do_not_change_fingerprint(self):
        (self.source / 'weights.safetensors').unlink()
        config = {'model_type': 'qwen3', 'hidden_size': 4096, 'num_hidden_layers': 36,
            'num_attention_heads': 32, 'num_key_value_heads': 8, 'quantization': {'bits': 4, 'group_size': 64}}
        for name, value in [('config.json', config), ('tokenizer.json', {}), ('tokenizer_config.json', {}),
                            ('model.safetensors.index.json', {'weight_map': {'fixture.a': 'shard-1.safetensors', 'fixture.b': 'shard-2.safetensors'}})]:
            (self.source / name).write_text(json.dumps(value))
        tensor_file(self.source / 'shard-1.safetensors', ['fixture.a'])
        tensor_file(self.source / 'shard-2.safetensors', ['fixture.b'])
        result = prepare.install('qwen3-8b', self.source, home=self.home)
        receipt = json.loads((Path(result['path']) / '.forum-model-install.json').read_text())
        digest = hashlib.sha256(b'forum-local-model-fingerprint-v1\0')
        for name, item in sorted(receipt['files'].items()):
            encoded = name.encode()
            digest.update(struct.pack('<Q', len(encoded)) + encoded + struct.pack('<Q', item['size_bytes']) + bytes.fromhex(item['sha256']))
        self.assertEqual(result['model_manifest_id'], 'sha256:' + digest.hexdigest())


if __name__ == '__main__':
    unittest.main()
