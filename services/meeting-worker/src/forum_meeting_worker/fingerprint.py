"""Same read-only content identity as forum-runtime, not a model registry."""
import hashlib
import os
from pathlib import Path
import struct

from .job_io import require


def model_fingerprint(directory, check=lambda: None):
    root = Path(directory).resolve(strict=True)
    require(root.is_dir(), 'Local model directory required.', 'MODEL_UNAVAILABLE')
    files, pending, visited = [], [root], 0
    while pending:
        visited += 1
        require(visited < 10000, 'Model directory traversal limit.', 'MODEL_UNAVAILABLE')
        for item in os.scandir(pending.pop()):
            if item.name.startswith('.'):
                continue
            path = Path(item.path)
            if item.is_dir(follow_symlinks=False):
                pending.append(path)
                continue
            if path.suffix in ('.json', '.safetensors', '.model', '.tiktoken', '.jinja') or item.name in ('vocab.txt', 'merges.txt', 'vocab', 'merges'):
                require(path.is_file(), 'Model artifact must be a regular file.', 'MODEL_UNAVAILABLE')
                files.append(path)
            require(len(files) + len(pending) < 10000, 'Model artifact limit.', 'MODEL_UNAVAILABLE')
    require(root / 'config.json' in files and any(p.suffix == '.safetensors' for p in files),
            'Model metadata or weights missing.', 'MODEL_UNAVAILABLE')
    manifest = hashlib.sha256(b'forum-local-model-fingerprint-v1\0')
    for path in sorted(files, key=lambda p: p.relative_to(root).as_posix()):
        check()
        name = path.relative_to(root).as_posix().encode('utf-8')
        content, count = hashlib.sha256(), 0
        with path.open('rb') as stream:
            before = os.fstat(stream.fileno())
            while data := stream.read(1024 * 1024):
                check()
                content.update(data)
                count += len(data)
            after = os.fstat(stream.fileno())
        require(count == before.st_size == after.st_size and before.st_mtime_ns == after.st_mtime_ns,
                'Model changed during fingerprinting.', 'MODEL_UNAVAILABLE')
        manifest.update(struct.pack('<Q', len(name)) + name + struct.pack('<Q', count) + content.digest())
    return 'sha256:' + manifest.hexdigest()
