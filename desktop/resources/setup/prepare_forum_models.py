#!/usr/bin/env python3
"""Copy explicitly prepared local models into Forum's own model directory.

Stdlib only. No downloads, model imports, source mutation or arbitrary destination
option. This verifies file/config integrity, not inference or meeting quality.
"""
from __future__ import annotations
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import shutil
import stat
import struct
import sys
import uuid

TARGETS = {'whisper': 'whisper-large-v3-turbo', 'qwen3-8b': 'qwen3-8b', 'ecapa': 'ecapa'}
ECAPA_SHA256 = '0575cb64845e6b9a10db9bcb74d5ac32b326b8dc90352671d345e2ee3d0126a2'
MAX_FILE_BYTES = 16 * 1024**3
MAX_TOTAL_BYTES = 24 * 1024**3
MAX_JSON_BYTES = 32 * 1024**2
MODEL_SUFFIXES = {'.json', '.safetensors', '.model', '.tiktoken', '.jinja'}
EXTRA_NAMES = {'vocab.txt', 'merges.txt', 'vocab', 'merges', 'README.md', 'LICENSE', 'LICENSE.txt', 'LICENSE.md', 'NOTICE', 'NOTICE.txt'}


class PreparationError(ValueError):
    pass


def require(condition, message):
    if not condition:
        raise PreparationError(message)


def strict_json(data):
    def pairs(items):
        output = {}
        for key, value in items:
            require(key not in output, 'JSON contains duplicate keys.')
            output[key] = value
        return output
    def constant(_):
        raise PreparationError('JSON contains a non-finite value.')
    return json.loads(data, object_pairs_hook=pairs, parse_constant=constant)


def read_small_json(path):
    with open_source(path) as stream:
        require(os.fstat(stream.fileno()).st_size <= MAX_JSON_BYTES, 'Model JSON is too large.')
        return strict_json(stream.read(MAX_JSON_BYTES + 1))


def open_source(path):
    # Symlinks are allowed only for read-only source files (HF cache blobs).
    descriptor = os.open(path, os.O_RDONLY | os.O_NONBLOCK)
    info = os.fstat(descriptor)
    if not stat.S_ISREG(info.st_mode) or not 0 < info.st_size <= MAX_FILE_BYTES:
        os.close(descriptor)
        raise PreparationError('Model input must be a nonempty bounded regular file: ' + path.name)
    return os.fdopen(descriptor, 'rb')


def tensor_names(path):
    with open_source(path) as stream:
        length = stream.read(8)
        require(len(length) == 8, 'Truncated safetensors header.')
        size, = struct.unpack('<Q', length)
        require(2 <= size <= 16 * 1024**2, 'Invalid safetensors header size.')
        header = strict_json(stream.read(size))
        require(isinstance(header, dict), 'Safetensors header must be an object.')
        data_size = os.fstat(stream.fileno()).st_size - 8 - size
        spans, names = [], set()
        widths = {'BOOL': 1, 'U8': 1, 'I8': 1, 'F8_E4M3': 1, 'F8_E5M2': 1,
                  'I16': 2, 'U16': 2, 'F16': 2, 'BF16': 2, 'I32': 4, 'U32': 4, 'F32': 4,
                  'I64': 8, 'U64': 8, 'F64': 8}
        for name, tensor in header.items():
            if name == '__metadata__':
                # The already validated MLX Whisper snapshot encodes absent
                # optional metadata as null; it has no tensor/loading semantics.
                require(tensor is None or (isinstance(tensor, dict) and all(isinstance(k, str) and isinstance(v, str) for k, v in tensor.items())), 'Invalid safetensors metadata.')
                continue
            require(isinstance(tensor, dict) and set(tensor) == {'dtype', 'shape', 'data_offsets'}, 'Invalid tensor header.')
            shape, offsets = tensor['shape'], tensor['data_offsets']
            require(isinstance(shape, list) and all(type(n) is int and 0 <= n <= 2**32 for n in shape) and len(shape) <= 16, 'Invalid tensor shape.')
            require(isinstance(offsets, list) and len(offsets) == 2 and all(type(n) is int for n in offsets) and 0 <= offsets[0] <= offsets[1] <= data_size, 'Tensor offsets exceed file.')
            require(isinstance(tensor['dtype'], str) and tensor['dtype'] in widths, 'Unsupported safetensors dtype.')
            count = 1
            for n in shape:
                count *= n
                require(count <= MAX_FILE_BYTES, 'Tensor dimensions exceed model file bounds.')
            require(count * widths[tensor['dtype']] == offsets[1] - offsets[0], 'Tensor shape/byte count mismatch.')
            spans.append(tuple(offsets)); names.add(name)
        require(bool(names), 'Safetensors file contains no weights.')
        end = 0
        for start, stop in sorted(spans):
            require(start == end, 'Safetensors data overlaps or has unclaimed bytes.')
            end = stop
        require(end == data_size, 'Safetensors payload is truncated or has trailing data.')
        return names


def inspect_model(source, kind):
    require(kind in TARGETS, 'Unsupported model type.')
    require(source.is_absolute() and source.is_dir(), 'Source must be an existing absolute local model directory.')
    if kind == 'ecapa':
        with open_source(source / 'embedding_model.ckpt'):
            pass
        return [source / 'embedding_model.ckpt', *sorted(path for path in source.iterdir() if path.name in EXTRA_NAMES)]
    config = read_small_json(source / 'config.json')
    require(isinstance(config, dict) and not config.get('auto_map'), 'Model config must not request custom Python code.')
    if kind == 'whisper':
        expected = {'model_type': 'whisper', 'n_mels': 128, 'n_audio_state': 1280, 'n_audio_layer': 32, 'n_text_state': 1280, 'n_text_layer': 4}
    else:
        expected = {'model_type': 'qwen3', 'hidden_size': 4096, 'num_hidden_layers': 36, 'num_attention_heads': 32, 'num_key_value_heads': 8}
    require(all(config.get(key) == value for key, value in expected.items()), 'Configuration does not match the supported ' + kind + ' architecture.')
    if kind == 'qwen3-8b':
        quant = config.get('quantization', config.get('quantization_config', {}))
        require(isinstance(quant, dict) and quant.get('bits') == 4 and quant.get('group_size') == 64, 'Qwen3 8B requires the supported MLX 4-bit/group-64 model.')
        require(isinstance(read_small_json(source / 'tokenizer.json'), dict), 'Tokenizer JSON is invalid.')
        tokenizer = read_small_json(source / 'tokenizer_config.json')
        require(isinstance(tokenizer, dict) and not tokenizer.get('auto_map'), 'Tokenizer must not request custom code.')
    files = []
    for item in source.iterdir():
        if item.name.startswith('.'):
            continue
        if item.suffix in MODEL_SUFFIXES or item.name in EXTRA_NAMES:
            require(not item.is_dir(), 'Nested model artifact directories are not supported by this installer.')
            files.append(item)
    require(len(files) <= 1000, 'Too many model artifacts.')
    weights = [path for path in files if path.suffix == '.safetensors']
    if kind == 'whisper':
        require({path.name for path in weights} == {'weights.safetensors'}, 'Whisper needs exactly weights.safetensors.')
    else:
        index_path = source / 'model.safetensors.index.json'
        if index_path.exists():
            index = read_small_json(index_path)
            weight_map = index.get('weight_map') if isinstance(index, dict) else None
            require(isinstance(weight_map, dict) and bool(weight_map), 'Model shard index is empty.')
            require(all(isinstance(k, str) and isinstance(v, str) and Path(v).name == v and v.endswith('.safetensors') for k, v in weight_map.items()), 'Shard index contains an unsafe filename.')
            require(set(weight_map.values()) == {path.name for path in weights}, 'Model shards are missing or not declared in the index.')
            for path in weights:
                require(tensor_names(path) == {key for key, value in weight_map.items() if value == path.name}, 'Shard tensor names differ from the index.')
        else:
            require({path.name for path in weights} == {'model.safetensors'}, 'Qwen3 requires model.safetensors or a complete shard index.')
    for path in weights:
        tensor_names(path)
    return sorted(files, key=lambda path: path.name)


def open_owned_root(home):
    require(home.is_absolute(), 'Home directory must be absolute.')
    root = home / 'Library/Application Support/AI Vision Forum/models'
    descriptor = os.open('/', os.O_RDONLY | os.O_DIRECTORY)
    try:
        for part in root.parts[1:]:
            try:
                child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=descriptor)
            except FileNotFoundError:
                os.mkdir(part, mode=0o700, dir_fd=descriptor)
                child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=descriptor)
            os.close(descriptor); descriptor = child
        require(os.fstat(descriptor).st_uid == os.getuid(), 'Forum models directory belongs to another user.')
        return root, descriptor
    except BaseException:
        os.close(descriptor)
        raise


def assert_owned_paths(root, root_fd, stage, stage_fd):
    """Reject a concurrently moved/replaced destination before publishing it."""
    descriptor = os.open('/', os.O_RDONLY | os.O_DIRECTORY)
    try:
        for part in root.parts[1:]:
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=descriptor)
            os.close(descriptor); descriptor = child
        actual, owned = os.fstat(descriptor), os.fstat(root_fd)
        require((actual.st_dev, actual.st_ino) == (owned.st_dev, owned.st_ino), 'Forum model directory changed during installation; refusing publication.')
        actual = os.stat(stage, dir_fd=descriptor, follow_symlinks=False)
        owned = os.fstat(stage_fd)
        require(stat.S_ISDIR(actual.st_mode) and (actual.st_dev, actual.st_ino) == (owned.st_dev, owned.st_ino), 'Model staging directory changed during installation.')
    finally:
        os.close(descriptor)


def install(kind, source, *, home=None, expected_manifest=None):
    if expected_manifest is not None:
        require(isinstance(expected_manifest, str) and expected_manifest.startswith('sha256:') and len(expected_manifest) == 71 and all(c in '0123456789abcdef' for c in expected_manifest[7:]), 'Expected manifest must be sha256:<64 lowercase hex>.')
    source = Path(source).expanduser()
    files = inspect_model(source, kind)
    root, root_fd = open_owned_root(Path.home() if home is None else Path(home))
    lock_fd, stage_fd, stage = None, None, None
    try:
        lock_fd = os.open('.model-install.lock', os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW | os.O_NONBLOCK, 0o600, dir_fd=root_fd)
        require(stat.S_ISREG(os.fstat(lock_fd).st_mode) and os.fstat(lock_fd).st_uid == os.getuid(), 'Invalid installation lock file.')
        try:
            fcntl.flock(lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise PreparationError('Another Forum model installation is running; retry after it finishes.') from None
        target = TARGETS[kind]
        try:
            os.stat(target, dir_fd=root_fd, follow_symlinks=False)
        except FileNotFoundError:
            pass
        else:
            raise PreparationError('Target already exists; no complete, partial or symlink target will be overwritten: ' + str(root / target))
        stage = '.install-' + uuid.uuid4().hex
        os.mkdir(stage, mode=0o700, dir_fd=root_fd)
        stage_fd = os.open(stage, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=root_fd)
        entries, total = {}, 0
        for path in files:
            require(Path(path.name).name == path.name and not path.name.startswith('.'), 'Invalid artifact filename.')
            with open_source(path) as original:
                before = os.fstat(original.fileno())
                total += before.st_size
                require(total <= MAX_TOTAL_BYTES, 'Model exceeds supported total size.')
                destination_fd = os.open(path.name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=stage_fd)
                digest, count = hashlib.sha256(), 0
                with os.fdopen(destination_fd, 'wb') as destination:
                    while data := original.read(1024 * 1024):
                        count += len(data)
                        require(count <= before.st_size, 'Source changed during copy.')
                        digest.update(data); destination.write(data)
                    destination.flush(); os.fsync(destination.fileno())
                after = os.fstat(original.fileno())
                require(count == before.st_size == after.st_size and before.st_mtime_ns == after.st_mtime_ns and before.st_ctime_ns == after.st_ctime_ns, 'Source changed during copy; candidate discarded.')
                entries[path.name] = {'sha256': digest.hexdigest(), 'size_bytes': count}
        if kind == 'ecapa':
            require(entries['embedding_model.ckpt']['sha256'] == ECAPA_SHA256, 'ECAPA checkpoint does not match the supported official model hash.')
            model_manifest = 'sha256:' + ECAPA_SHA256
        else:
            digest = hashlib.sha256(b'forum-local-model-fingerprint-v1\0')
            for name, entry in sorted(entries.items()):
                if Path(name).suffix not in MODEL_SUFFIXES and name not in {'vocab.txt', 'merges.txt', 'vocab', 'merges'}:
                    continue
                encoded = name.encode('utf8')
                digest.update(struct.pack('<Q', len(encoded)) + encoded + struct.pack('<Q', entry['size_bytes']) + bytes.fromhex(entry['sha256']))
            model_manifest = 'sha256:' + digest.hexdigest()
        if expected_manifest is not None:
            require(model_manifest == expected_manifest, 'Copied model fingerprint differs from the expected source manifest.')
        # Validate copied config/header bytes again, never publish an interrupted
        # or inconsistent copy. Directory descriptors retain the owned stage.
        stage_path = root / stage
        assert_owned_paths(root, root_fd, stage, stage_fd)
        inspect_model(stage_path, kind)
        for name, entry in entries.items():
            digest = hashlib.sha256()
            with open_source(stage_path / name) as stream:
                for data in iter(lambda: stream.read(1024 * 1024), b''):
                    digest.update(data)
            require(digest.hexdigest() == entry['sha256'], 'Copied model verification failed.')
        receipt = {'schema_version': 1, 'model_type': kind, 'model_manifest_id': model_manifest, 'files': entries,
            'source': str(source), 'model_downloaded': False, 'inference_validated': False}
        descriptor = os.open('.forum-model-install.json', os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600, dir_fd=stage_fd)
        with os.fdopen(descriptor, 'w') as destination:
            json.dump(receipt, destination, ensure_ascii=False, indent=2); destination.write('\n')
            destination.flush(); os.fsync(destination.fileno())
        os.fsync(stage_fd)
        assert_owned_paths(root, root_fd, stage, stage_fd)
        # renamex_np(RENAME_EXCL) is macOS's atomic no-replace rename. This
        # prevents a concurrently created empty directory from being replaced.
        publish_exclusive(root_fd, stage, target)
        stage = target  # Still owned until the destination identity is confirmed.
        os.fsync(root_fd)
        assert_owned_paths(root, root_fd, stage, stage_fd)
        stage = None
        return {'status': 'installed', 'type': kind, 'path': str(root / target), 'model_manifest_id': model_manifest,
            'files': len(entries), 'bytes_copied': total, 'inference_validated': False}
    finally:
        if stage is not None and stage_fd is not None:
            # Cleanup only this invocation's random stage; never source/target.
            try:
                actual, owned = os.stat(stage, dir_fd=root_fd, follow_symlinks=False), os.fstat(stage_fd)
                if (actual.st_dev, actual.st_ino) == (owned.st_dev, owned.st_ino):
                    shutil.rmtree(stage, dir_fd=root_fd)
            except FileNotFoundError:
                pass
        if stage_fd is not None:
            os.close(stage_fd)
        if lock_fd is not None:
            os.close(lock_fd)
        os.close(root_fd)


def publish_exclusive(root_fd, stage, target):
    import ctypes
    library = ctypes.CDLL(None, use_errno=True)
    function = library.renameatx_np
    function.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
    function.restype = ctypes.c_int
    if function(root_fd, stage.encode(), root_fd, target.encode(), 0x4) != 0:  # RENAME_EXCL
        error = ctypes.get_errno()
        raise OSError(error, os.strerror(error))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--type', required=True, choices=sorted(TARGETS))
    parser.add_argument('--source', required=True, type=Path, help='Explicit prepared local model directory; never modified.')
    parser.add_argument('--expected-manifest', help='Optional sha256:<hash> fingerprint from the prepared model receipt.')
    args = parser.parse_args()
    if sys.platform != 'darwin':
        parser.error('This preparation tool targets macOS.')
    try:
        result = install(args.type, args.source, expected_manifest=args.expected_manifest)
    except (PreparationError, OSError, ValueError, KeyError) as exc:
        print('模型准备失败：' + str(exc), file=sys.stderr)
        return 1
    print(json.dumps(result, ensure_ascii=False, indent=2))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
