#!/usr/bin/env python3
"""Build an independent portable speaker runtime from pinned official artifacts.

No pre-existing Python environment is copied, changed or hard-linked. No model
weights are packaged or downloaded. Existing output paths are never replaced.
"""
from __future__ import annotations
import argparse
import base64
import csv
import importlib.util
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tarfile
import tempfile

REPO = Path(__file__).resolve().parents[2]
WORKER = REPO / 'services/speaker-worker'
LOCK = WORKER / 'packaging/runtime-macos-arm64.lock.json'
OWNER = 'ai-vision-forum-speaker-worker-v1'

# Reuse the existing reviewed artifact/path/hash/portable-Python primitives.
# Its meeting-worker package() is never called and its globals are not changed.
HELPER = Path(__file__).with_name('package_meeting_worker.py')
_spec = importlib.util.spec_from_file_location('forum_portable_primitives', HELPER)
base = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(base)


def checker_environment(env):
    # The build uses a sanitized environment; forward only this documented knob.
    key = 'FORUM_SPEAKER_CHECK_TIMEOUT_SECONDS'
    return {**env, **({key: os.environ[key]} if key in os.environ else {})}


def validate_lock(lock):
    if lock.get('schema_version') != 1 or lock.get('target') != 'aarch64-apple-darwin' or lock.get('component') != 'speaker-worker' or lock.get('model_weights_included') is not False:
        raise ValueError('Unexpected speaker lock identity.')
    for artifact in [lock['python'], lock['python_license_archive'], *lock['wheels']]:
        base.validate_artifact(artifact)
    runtime = [wheel for wheel in lock['wheels'] if wheel['role'] == 'runtime']
    names = [wheel['name'].lower().replace('_', '-').replace('.', '-') for wheel in runtime]
    if len(set(names)) != len(names) or not {'torch', 'torchaudio', 'speechbrain', 'numpy'} <= set(names):
        raise ValueError('Speaker lock requires unique runtime names and its four ML packages.')
    if any(wheel['role'] not in {'build', 'runtime'} for wheel in lock['wheels']):
        raise ValueError('Unknown wheel role.')



def relocate_torchaudio(site):
    """Replace two known wheel build-host rpaths with the bundled Torch lib path."""
    checker_path = Path(__file__).with_name('check_meeting_worker_bundle.py')
    spec = importlib.util.spec_from_file_location('speaker_relocation_check', checker_path)
    checker = importlib.util.module_from_spec(spec); spec.loader.exec_module(checker)
    previous = '/Users/ec2-user/runner/_work/_temp/conda_environment_23408961961/lib'
    replacement = '@loader_path/../../torch/lib'
    changes = []
    record = site / 'torchaudio-2.11.0.dist-info/RECORD'
    with record.open(newline='') as stream:
        rows = list(csv.reader(stream))
    for name in ('_torchaudio.abi3.so', 'libtorchaudio.abi3.so'):
        target = site / 'torchaudio/lib' / name
        info = checker.macho_arm64(target)
        if info['rpaths'] != [previous]:
            raise ValueError('Pinned Torchaudio wheel changed expected relocation inputs.')
        for dependency in info['dependencies']:
            if dependency.startswith('@rpath/') and not (site / 'torch/lib' / dependency.removeprefix('@rpath/')).is_file():
                raise ValueError('Torchaudio dependency is absent from bundled Torch.')
        before = base.sha256(target)
        subprocess.run(['/usr/bin/install_name_tool', '-rpath', previous, replacement, str(target)], check=True, stdout=sys.stderr, stderr=sys.stderr)
        subprocess.run(['/usr/bin/codesign', '--force', '--sign', '-', '--timestamp=none', str(target)], check=True, stdout=sys.stderr, stderr=sys.stderr)
        after = base.sha256(target); relative = str(target.relative_to(site))
        matches = [row for row in rows if row[0] == relative]
        if len(matches) != 1:
            raise ValueError('Torchaudio RECORD is missing the relocated library.')
        matches[0][1:] = ['sha256=' + base64.urlsafe_b64encode(bytes.fromhex(after)).decode().rstrip('='), str(target.stat().st_size)]
        changes.append({'file': relative, 'original_sha256': before, 'bundled_sha256': after,
            'removed_build_rpaths': [previous], 'added_relative_rpaths': [replacement], 'signature': 'ad-hoc, timestamp disabled'})
    with record.open('w', newline='') as stream:
        csv.writer(stream).writerows(rows)
    return changes


def copy_python_licenses(python, staged, scratch, build_site, archive_path, env):
    archive_tar = scratch / 'python-full.tar'
    base.run(python, ['-c', 'import sys;sys.path.insert(0,sys.argv[1]);import zstandard;'
        'source=open(sys.argv[2],"rb");destination=open(sys.argv[3],"wb");'
        'zstandard.ZstdDecompressor().copy_stream(source,destination);destination.close()',
        str(build_site), str(archive_path), str(archive_tar)], cwd=scratch, env=env)
    output = staged / 'third-party/python'; output.mkdir(parents=True)
    with tarfile.open(archive_tar, 'r:') as archive:
        names = [member.name for member in archive.getmembers() if member.name.startswith('python/licenses/') and member.isfile()]
        if not names:
            raise ValueError('Upstream Python license archive contains no license files.')
        for name in ['python/PYTHON.json', *names]:
            member = archive.getmember(name)
            if '..' in Path(name).parts or not member.isfile() or member.size > 2 * 1024 * 1024:
                raise ValueError('Invalid upstream license text.')
            with archive.extractfile(member) as source:
                (output / Path(name).name).write_bytes(source.read())
    return [Path(name).name for name in names]


def package(output, cache, offline=False):
    output = base.new_output(output)
    if cache.is_symlink():
        raise ValueError('Cache must not be a symlink.')
    cache.mkdir(parents=True, exist_ok=True); cache = cache.resolve(strict=True)
    lock = json.loads(LOCK.read_text()); validate_lock(lock)
    artifacts = [lock['python'], lock['python_license_archive'], *lock['wheels']]
    downloads = {artifact['filename']: base.fetch(artifact, cache, offline) for artifact in artifacts}
    with tempfile.TemporaryDirectory(prefix='.forum-speaker-build-', dir=output.parent) as directory:
        scratch = Path(directory); staged = scratch / 'speaker-worker'; staged.mkdir()
        env = base.isolated_environment(scratch, lock['source_date_epoch'])
        with tarfile.open(downloads[lock['python']['filename']], 'r:gz') as archive:
            if any(not (member.name == 'python' or member.name.startswith('python/')) for member in archive.getmembers()):
                raise ValueError('Unexpected standalone Python archive layout.')
            archive.extractall(staged, filter='data')
        python = staged / 'python/bin/python3.12'
        build_site = scratch / 'build-site'
        base.install(python, [w for w in lock['wheels'] if w['role'] == 'build'], cache, build_site, scratch, env, 'build')
        source = scratch / 'source'; source.mkdir()
        source_hashes = {}
        for original in [WORKER / 'pyproject.toml', WORKER / 'README.md', *sorted((WORKER / 'src').rglob('*.py'))]:
            if original.is_symlink():
                raise ValueError('Speaker source must not be a symlink.')
            relative = original.relative_to(WORKER); destination = source / relative
            destination.parent.mkdir(parents=True, exist_ok=True); shutil.copyfile(original, destination)
            source_hashes[str(relative)] = base.sha256(original)
        wheelhouse = scratch / 'wheelhouse'; wheelhouse.mkdir()
        base.run(python, ['-c', 'import sys;sys.path.insert(0,sys.argv[1]);from setuptools.build_meta import build_wheel;build_wheel(sys.argv[2])',
            str(build_site), str(wheelhouse)], cwd=source, env=env)
        worker_wheel, = wheelhouse.glob('forum_speaker_worker-*.whl'); version = worker_wheel.name.split('-')[1]
        site = staged / 'python/lib/python3.12/site-packages'
        base.install(python, [w for w in lock['wheels'] if w['role'] == 'runtime'], cache, site, scratch, env, 'runtime')
        if (site / 'bin').is_dir():
            shutil.rmtree(site / 'bin')
        base.install(python, [{'name': 'forum-speaker-worker', 'version': version, 'sha256': base.sha256(worker_wheel)}], wheelhouse, site, scratch, env, 'worker')
        base.run(python, ['-m', 'pip', '--isolated', 'check'], cwd=scratch, env=env)
        native_relocations = base.relocate_whisper_dependencies(site) + relocate_torchaudio(site)
        license_files = copy_python_licenses(python, staged, scratch, build_site, downloads[lock['python_license_archive']['filename']], env)
        for item in (site / 'bin', *site.glob('pip*')):
            if item.is_dir() and not item.is_symlink():
                shutil.rmtree(item)
            elif item.exists() or item.is_symlink():
                item.unlink()
        for item in (staged / 'python/bin').iterdir():
            if item.name not in {'python', 'python3', 'python3.12'}:
                item.unlink()
        shutil.rmtree(staged / 'python/lib/python3.12/ensurepip')
        for record in site.glob('*.dist-info/RECORD'):
            with record.open(newline='') as stream:
                rows = [row for row in csv.reader(stream) if not row[0].startswith('../../bin/')]
            with record.open('w', newline='') as stream:
                csv.writer(stream).writerows(rows)
        launcher_dir = staged / 'bin'; launcher_dir.mkdir()
        launcher = launcher_dir / 'speaker-worker'
        launcher.write_text('#!/bin/sh\nset -eu\n'
            'worker_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)\n'
            'unset PYTHONHOME PYTHONPATH VIRTUAL_ENV CONDA_PREFIX DYLD_LIBRARY_PATH DYLD_FALLBACK_LIBRARY_PATH\n'
            'exec "$worker_root/python/bin/python3.12" -I -B -m forum_speaker_worker "$@"\n')
        launcher.chmod(0o755)
        checks = staged / 'checks'; checks.mkdir()
        checker = WORKER / 'packaging/check_bundle.py'
        native_checker = Path(__file__).with_name('check_meeting_worker_bundle.py')
        shutil.copyfile(checker, checks / 'check_bundle.py')
        shutil.copyfile(native_checker, checks / '_native_check.py')
        notices = ['# Speaker runtime third-party inventory', '',
            'No model weights are included. The optional ECAPA model must be separately installed and explicitly granted by the host.',
            'Standalone CPython and static-library license texts: third-party/python/.',
            'Original wheel license/NOTICE files remain in python/lib/python3.12/site-packages/.', '', '## Runtime wheels', '']
        for wheel in lock['wheels']:
            if wheel['role'] == 'runtime':
                notices.append(f"- {wheel['name']} {wheel['version']}: {wheel['metadata_url']}")
        (staged / 'THIRD_PARTY.md').write_text('\n'.join(notices) + '\n')
        manifest = {'owner': OWNER, 'schema_version': 1, 'target': lock['target'], 'minimum_macos': lock['minimum_macos'],
            'python': lock['python'], 'worker_version': version, 'worker_wheel_sha256': base.sha256(worker_wheel),
            'source_sha256': source_hashes, 'lock_sha256': base.sha256(LOCK), 'runtime_wheels': [w for w in lock['wheels'] if w['role'] == 'runtime'],
            'build_wheels': [w for w in lock['wheels'] if w['role'] == 'build'], 'python_license_archive': lock['python_license_archive'],
            'python_license_files': license_files, 'packaging_script_sha256': base.sha256(Path(__file__)),
            'packaging_primitives_sha256': base.sha256(HELPER), 'check_script_sha256': base.sha256(checker),
            'native_check_script_sha256': base.sha256(native_checker), 'entrypoint': 'bin/speaker-worker',
            'task_types': ['speaker_embedding'], 'model_weights_included': False, 'formal_product_acceptance': 'not_evaluated',
            'native_relocations': native_relocations}
        (staged / 'runtime-manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
        base.run(python, [str(checks / 'check_bundle.py'), '--root', str(staged)], cwd=scratch, env=checker_environment(env))
        if output.exists() or output.is_symlink():
            raise ValueError('Output appeared during packaging; refusing replacement.')
        staged.rename(output)
    return {'output': str(output), 'launcher': str(output / 'bin/speaker-worker'), 'python': str(output / 'python/bin/python3.12'),
        'model_weights_included': False, 'lock_sha256': base.sha256(LOCK)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--cache', required=True, type=Path)
    parser.add_argument('--offline', action='store_true')
    args = parser.parse_args()
    if sys.version_info < (3, 12) or platform.system() != 'Darwin' or platform.machine() != 'arm64':
        parser.error('Build on macOS arm64 with a Python 3.12+ bootstrap interpreter.')
    try:
        result = package(args.output, args.cache, args.offline)
    except (ValueError, OSError, subprocess.CalledProcessError, tarfile.TarError) as exc:
        print(f'Speaker packaging failed: {exc}', file=sys.stderr)
        return 1
    print(json.dumps(result, indent=2))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
