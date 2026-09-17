#!/usr/bin/env python3
"""Verify a standalone speaker runtime without a model or developer Python."""
from __future__ import annotations
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
from uuid import uuid4

OWNER = 'ai-vision-forum-speaker-worker-v1'
SITE = Path('python/lib/python3.12/site-packages')


def digest(path):
    value = hashlib.sha256()
    with path.open('rb') as stream:
        for data in iter(lambda: stream.read(1024 * 1024), b''):
            value.update(data)
    return value.hexdigest()


def check_paths(root):
    manifest = json.loads((root / 'runtime-manifest.json').read_text())
    if manifest.get('owner') != OWNER or manifest.get('schema_version') != 1 or manifest.get('target') != 'aarch64-apple-darwin' or manifest.get('task_types') != ['speaker_embedding'] or manifest.get('model_weights_included') is not False or manifest.get('formal_product_acceptance') != 'not_evaluated':
        raise ValueError('Speaker manifest identity or acceptance claim is invalid.')
    if manifest.get('entrypoint') != 'bin/speaker-worker':
        raise ValueError('Unexpected speaker entrypoint.')
    for path in root.rglob('*'):
        if path.is_symlink() and not path.resolve().is_relative_to(root):
            raise ValueError('Bundle symlink escapes runtime: ' + str(path.relative_to(root)))
        if path.suffix in {'.ckpt', '.safetensors'}:
            raise ValueError('Optional model weights must not be bundled.')
    source_hashes = manifest.get('source_sha256')
    if not isinstance(source_hashes, dict):
        raise ValueError('Source hashes missing.')
    expected = set()
    for name, expected_digest in source_hashes.items():
        relative = Path(name)
        if relative.is_absolute() or '..' in relative.parts:
            raise ValueError('Unsafe source digest path.')
        if name.startswith('src/'):
            expected.add(str(Path(*relative.parts[1:])))
            path = root / SITE / Path(*relative.parts[1:])
            if path.is_symlink() or not path.is_file() or digest(path) != expected_digest:
                raise ValueError('Installed speaker source digest mismatch: ' + name)
    actual = {str(path.relative_to(root / SITE)) for path in (root / SITE / 'forum_speaker_worker').rglob('*.py')}
    if not expected or actual != expected:
        raise ValueError('Installed speaker module files differ from the manifest.')
    for relative, key in [('checks/check_bundle.py', 'check_script_sha256'), ('checks/_native_check.py', 'native_check_script_sha256')]:
        if digest(root / relative) != manifest.get(key):
            raise ValueError('Bundled checker digest mismatch.')
    if not (root / 'third-party/python/PYTHON.json').is_file() or not manifest.get('python_license_files'):
        raise ValueError('Standalone Python licenses are missing.')
    for name in manifest['python_license_files']:
        if not isinstance(name, str) or Path(name).name != name or not (root / 'third-party/python' / name).is_file():
            raise ValueError('A declared Python license file is missing or unsafe.')
    return manifest


def check(root):
    root = root.resolve(strict=True)
    manifest = check_paths(root)
    helper = root / 'checks/_native_check.py'
    spec = importlib.util.spec_from_file_location('native_speaker_bundle_check', helper)
    native = importlib.util.module_from_spec(spec); spec.loader.exec_module(native)
    native_result = native.check_native_dependencies(root)
    with tempfile.TemporaryDirectory(prefix='forum-speaker-isolation-') as directory:
        scratch = Path(directory); env = native.isolated_environment(scratch)
        python = root / 'python/bin/python3.12'
        def run(arguments, **kwargs):
            return subprocess.run([str(python), '-I', '-B', *arguments], cwd=scratch, env=env,
                text=True, capture_output=True, timeout=30, check=True, **kwargs)
        runtime = json.loads(run(['-c', native.RUNTIME_INFO, str(root)]).stdout)
        if runtime['python_version'] != manifest['python']['version'] or runtime['machine'] != 'arm64':
            raise ValueError('Interpreter version or architecture differs from manifest.')
        versions = runtime['versions']
        for wheel in manifest['runtime_wheels']:
            key = wheel['name'].lower().replace('_', '-').replace('.', '-')
            observed = {name.replace('.', '-'): version for name, version in versions.items()}
            if observed.get(key) != wheel['version']:
                raise ValueError('Installed distribution version differs from lock: ' + wheel['name'])
        # Imports verify native linking without loading weights or opening a hub.
        imports = run(['-c', 'import sys,json;out=sys.stdout;sys.stdout=sys.stderr;'
            'import torch,torchaudio,speechbrain;from speechbrain.lobes.models.ECAPA_TDNN import ECAPA_TDNN;'
            'torch.set_num_threads(2);sys.stdout=out;print(json.dumps({"torch":torch.__version__,"speechbrain":speechbrain.__version__,"cpu_threads":torch.get_num_threads()}))'])
        model = scratch / 'unused-model'; model.mkdir()
        initialize = {'protocol_version': 1, 'instance_id': str(uuid4()), 'job_root': str(scratch),
            'model_grant': {'profile': 'ecapa-voxceleb-v1', 'model_path': str(model), 'model_manifest_id': 'sha256:' + '0' * 64}}
        requests = [{'jsonrpc': '2.0', 'id': identifier, 'method': method, 'params': params}
            for identifier, method, params in [(1, 'initialize', initialize), (2, 'health.ping', {}), (3, 'jobs.run', {}), (4, 'shutdown', {})]]
        output = run(['-m', 'forum_speaker_worker'], input=''.join(json.dumps(request) + '\n' for request in requests))
        replies = [json.loads(line) for line in output.stdout.splitlines()]
        if [reply.get('id') for reply in replies] != [1, 2, 3, 4]:
            raise ValueError('Speaker lifecycle response IDs do not match.')
        if replies[0].get('result', {}).get('capabilities') != {'health': True, 'task_types': ['speaker_embedding'], 'model_clients': ['local-ecapa']} or replies[1].get('result', {}).get('status') != 'ok' or replies[2].get('error', {}).get('data', {}).get('code') != 'INVALID_PARAMS' or replies[3].get('result', {}).get('status') != 'shutdown_requested':
            raise ValueError('Speaker lifecycle contract failed.')
    return {'status': 'passed', 'runtime': runtime, 'native_dependencies': native_result,
        'model_imports': json.loads(imports.stdout), 'lifecycle': replies,
        'model_weights_loaded': False, 'formal_clean_mac_acceptance': False}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', required=True, type=Path)
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    result = check(args.root)
    data = json.dumps(result, indent=2) + '\n'
    if args.output:
        args.output.write_text(data)
    print(data)


if __name__ == '__main__':
    main()
