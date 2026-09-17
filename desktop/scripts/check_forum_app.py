#!/usr/bin/env python3
"""Verify a built Forum app's runtime inputs and actual macOS signing state.

No audio or model is opened. --runtime-checks invokes each packaged runtime's
isolated dependency/lifecycle checker. Developer ID signing does not imply Apple
notarization; those states are reported separately.
"""
from __future__ import annotations
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import plistlib
import re
import subprocess
import sys

OWNER = 'ai-vision-forum-app-v1'
RUNTIME_SITE = Path('python/lib/python3.12/site-packages')
INPUTS = {
    'meeting-runtime.lock.json': 'services/asr-worker/runtime-macos-arm64.lock.json',
    'speaker-runtime.lock.json': 'services/speaker-worker/packaging/runtime-macos-arm64.lock.json',
    'Cargo.lock': 'desktop/Cargo.lock',
    'package-lock.json': 'desktop/forum-shell/ui/package-lock.json',
    'forum.schema.json': 'packages/contracts/forum.schema.json',
    'forum-profile.json': 'profiles/ai-vision-forum/profile.json',
    'prepare_forum_models.py': 'desktop/resources/setup/prepare_forum_models.py',
}


def sha(path):
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(block)
    return digest.hexdigest()


def signature_kind(output):
    if 'Signature=adhoc' in output:
        return 'ad_hoc_development'
    if re.search(r'^Authority=Developer ID Application:', output, re.M) and re.search(r'^TeamIdentifier=(?!not set)[A-Z0-9]+$', output, re.M) and re.search(r'^CodeDirectory .*flags=.*\bruntime\b', output, re.M):
        return 'developer_id_application'
    return 'unrecognized'


def validate_manifest(manifest):
    if manifest.get('owner') != OWNER or manifest.get('schema_version') != 1 or manifest.get('profile') not in {'dev', 'release'} or manifest.get('model_weights_included') is not False:
        raise ValueError('Invalid app release manifest.')
    expected = 'ad_hoc_development' if manifest['profile'] == 'dev' else 'developer_id_application'
    if manifest.get('signing_policy') != expected or manifest.get('notarization') != 'not_submitted':
        raise ValueError('Build manifest must distinguish signing from later notarization.')
    if not isinstance(manifest.get('input_sha256'), dict) or set(manifest['input_sha256']) != set(INPUTS):
        raise ValueError('Missing pinned app inputs.')
    return expected


def check_runtime_sources(resource, manifest, source_root=None):
    for name, expected in manifest['source_sha256'].items():
        relative = Path(name)
        if relative.is_absolute() or '..' in relative.parts:
            raise ValueError('Unsafe runtime source path.')
        if name.startswith('src/'):
            actual = resource / RUNTIME_SITE / Path(*relative.parts[1:])
            if actual.is_symlink() or not actual.is_file() or sha(actual) != expected:
                raise ValueError('Installed runtime source differs from package manifest: ' + name)
        if source_root is not None and (not (source_root / relative).is_file() or sha(source_root / relative) != expected):
            raise ValueError('Runtime is stale compared with current source: ' + name)


def check(app, expected_profile=None, repo=None, runtime_checks=False, allow_unsigned=False):
    app = app.resolve(strict=True); resources = app / 'Contents/Resources'
    manifest = json.loads((resources / 'release-manifest.json').read_text())
    expected_signing = validate_manifest(manifest)
    if expected_profile is not None and manifest['profile'] != expected_profile:
        raise ValueError('App profile does not match requested distribution profile.')
    with (app / 'Contents/Info.plist').open('rb') as stream:
        info = plistlib.load(stream)
    if info.get('CFBundleIdentifier') != 'org.aivisionforum.agent' or info.get('CFBundleShortVersionString') != manifest['marketing_version'] or info.get('CFBundleVersion') != manifest['build_version']:
        raise ValueError('App identity/version differs from build manifest.')
    for name, repository_path in INPUTS.items():
        actual = resources / 'build-inputs' / name
        if actual.is_symlink() or sha(actual) != manifest['input_sha256'][name]:
            raise ValueError('Bundled dependency lock hash mismatch: ' + name)
        if repo is not None and sha(repo / repository_path) != manifest['input_sha256'][name]:
            raise ValueError('App lock is stale compared with repository: ' + name)
    if sha(resources / 'setup/prepare_forum_models.py') != manifest['input_sha256']['prepare_forum_models.py']:
        raise ValueError('Bundled offline model preparation tool differs from the checked build input.')
    component_reports = {}
    for component in ('meeting-worker', 'speaker-worker'):
        resource = resources / component
        component_manifest = json.loads((resource / 'runtime-manifest.json').read_text())
        owner = 'ai-vision-forum-meeting-worker-v1' if component == 'meeting-worker' else 'ai-vision-forum-speaker-worker-v1'
        if component_manifest.get('owner') != owner or not isinstance(component_manifest.get('source_sha256'), dict) or not component_manifest['source_sha256']:
            raise ValueError('Runtime component identity/source digest map is invalid.')
        lock_name = 'meeting-runtime.lock.json' if component == 'meeting-worker' else 'speaker-runtime.lock.json'
        if component_manifest['lock_sha256'] != manifest['input_sha256'][lock_name]:
            raise ValueError('Packaged runtime does not match the bundled dependency lock.')
        source_root = None if repo is None else repo / 'services' / component
        check_runtime_sources(resource, component_manifest, source_root)
        component_reports[component] = {'lock_sha256': component_manifest['lock_sha256'], 'worker_version': component_manifest['worker_version']}
        if component == 'meeting-worker':
            asr = component_manifest.get('asr_adapter')
            if not isinstance(asr, dict) or asr.get('model_weights_included') is not False or sha(resource / 'asr/asr_worker.py') != asr.get('script_sha256'):
                raise ValueError('Bundled ASR adapter is absent or does not match its source hash.')
            if repo is not None and sha(repo / 'services/asr-worker/asr_worker.py') != asr['script_sha256']:
                raise ValueError('ASR adapter is stale compared with repository.')
            component_reports['asr-worker'] = {'process_isolation': 'separate_process', 'python_environment': 'meeting-worker', 'script_sha256': asr['script_sha256']}
        if runtime_checks:
            python = resource / 'python/bin/python3.12'
            if component == 'meeting-worker':
                command = [str(python), '-I', '-B', str(resource / 'checks/check_meeting_worker_bundle.py'), '--resource-dir', str(resource)]
            else:
                command = [str(python), '-I', '-B', str(resource / 'checks/check_bundle.py'), '--root', str(resource)]
            result = subprocess.run(command, check=True, capture_output=True, text=True, timeout=90,
                env={'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'LANG': 'en_US.UTF-8'})
            component_reports[component]['isolated_check'] = json.loads(result.stdout)
    native_path = resources / 'speaker-worker/checks/_native_check.py'
    spec = importlib.util.spec_from_file_location('forum_app_native', native_path)
    native = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(native)
    host_natives = [app / 'Contents/MacOS' / name for name in ('forum-shell', 'dora', 'dora-qwen3-asr', 'dora-qwen35-translator', 'hen-local-init')]
    if (resources / 'lib/libAudioCapture.dylib').exists():
        raise ValueError('Legacy macOS-15 native AEC is not part of the Forum reliable capture bundle.')
    for path in host_natives:
        info_native = native.macho_arm64(path)
        if info_native['minimum_macos'] > (14, 0, 0):
            raise ValueError('Host native executable requires newer than macOS 14: ' + path.name)
        for dependency in [*info_native['dependencies'], *info_native['rpaths']]:
            if not dependency.startswith(('/System/Library/', '/usr/lib/', '@rpath/', '@loader_path', '@executable_path')):
                raise ValueError('Host executable has a nonportable dependency: ' + path.name)
    signing = 'not_checked'
    if not allow_unsigned:
        subprocess.run(['/usr/bin/codesign', '--verify', '--deep', '--strict', str(app)], check=True, capture_output=True, text=True)
        result = subprocess.run(['/usr/bin/codesign', '--display', '--verbose=4', str(app)], check=True, capture_output=True, text=True)
        signing = signature_kind(result.stdout + result.stderr)
        if signing != expected_signing:
            raise ValueError('Actual app signature does not match the requested dev/release profile: ' + signing)
    return {'status': 'passed', 'profile': manifest['profile'], 'version': manifest['version'], 'signature': signing,
        'notarization': 'not_checked', 'formal_clean_mac_acceptance': False, 'model_weights_included': False,
        'host_native_files': [str(path.relative_to(app)) for path in host_natives], 'components': component_reports}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app', required=True, type=Path)
    parser.add_argument('--profile', choices=['dev', 'release'])
    parser.add_argument('--repo', type=Path, help='Build-only check against current repository sources.')
    parser.add_argument('--runtime-checks', action='store_true')
    parser.add_argument('--allow-unsigned', action='store_true', help='Build-only pre-signing validation; never a release gate.')
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    try:
        result = check(args.app, args.profile, args.repo, args.runtime_checks, args.allow_unsigned)
    except (ValueError, OSError, KeyError, subprocess.SubprocessError) as exc:
        print('Forum app validation failed: ' + str(exc), file=sys.stderr)
        return 1
    data = json.dumps(result, indent=2) + '\n'
    if args.output:
        args.output.write_text(data)
    print(data)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
