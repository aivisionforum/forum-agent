#!/usr/bin/env python3
"""Model-free integration evidence. Never claims physical/release acceptance."""
from __future__ import annotations
import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
SOURCE_SCOPES = ['desktop', 'services/meeting-worker', 'services/speaker-worker',
                 'packages/contracts', 'profiles', 'scripts/run_forum_regression.py',
                 '.github/workflows/forum-regression.yml', 'docs/engineering/acceptance-profile.json']

def source_hashes():
    paths = subprocess.check_output(['git', 'ls-files', '-z', '--cached', '--others',
                                     '--exclude-standard', '--', *SOURCE_SCOPES], cwd=ROOT)
    return {name: hashlib.sha256((ROOT / name).read_bytes()).hexdigest()
            for name in sorted(set(paths.decode().split('\0')))
            if name and (ROOT / name).is_file()}

def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--output', required=True, type=Path, help='new absolute output directory')
    p.add_argument('--python', default=shutil.which('python3.12') or sys.executable)
    p.add_argument('--online', action='store_true', help='permit Cargo dependency fetches; never model downloads')
    p.add_argument('--portable', action='store_true', help='core/runtime/gateway/contracts and workers only, for CI')
    args = p.parse_args()
    if not args.output.is_absolute() or args.output.exists():
        p.error('--output must be a new absolute directory')
    args.output.mkdir(parents=True, mode=0o700)
    before = source_hashes()
    acceptance_file=ROOT / 'docs/engineering/acceptance-profile.json'
    acceptance=json.loads(acceptance_file.read_text())
    env = os.environ.copy()
    env.update(PYTHONNOUSERSITE='1', HF_HUB_OFFLINE='1', TRANSFORMERS_OFFLINE='1')
    cargo = ['cargo', '+stable']
    locked = ['--locked'] + ([] if args.online else ['--offline'])
    commands = [
        ('core-runtime-gateway', ROOT / 'desktop', cargo + ['test', '-p', 'forum-contracts', '-p', 'forum-core', '-p', 'forum-runtime', '-p', 'forum-gateway', '--lib', '--tests', *locked], {}),
        ('generated-contracts', ROOT / 'desktop', cargo + ['run', '-p', 'forum-core', '--example', 'generate_contracts', *locked, '--', '--check'], {}),
        ('meeting-worker', ROOT, [args.python, '-m', 'unittest', 'discover', '-s', 'services/meeting-worker/tests', '-v'], {'PYTHONPATH': str(ROOT / 'services/meeting-worker/src')}),
        ('speaker-worker', ROOT, [args.python, '-m', 'unittest', 'discover', '-s', 'services/speaker-worker/tests', '-v'], {'PYTHONPATH': str(ROOT / 'services/speaker-worker/src')}),
        ('profile', ROOT, [args.python, 'scripts/validate_forum_profile.py'], {}),
    ]
    if not args.portable:
        commands += [
            ('release-tools', ROOT, [args.python, '-m', 'unittest', 'discover', '-s', 'desktop/scripts/tests', '-v'], {}),
            ('audio-bridge', ROOT / 'desktop', cargo + ['test', '-p', 'moxin-dora-bridge', '--lib', *locked], {}),
            ('desktop-host', ROOT / 'desktop', cargo + ['test', '-p', 'forum-shell', '--bin', 'forum-shell', *locked], {}),
            ('ui-check', ROOT / 'desktop/forum-shell/ui', ['npm', 'run', 'check'], {}),
            ('ui-tests', ROOT / 'desktop/forum-shell/ui', ['npm', 'run', 'test:p0'], {}),
            ('ui-build', ROOT / 'desktop/forum-shell/ui', ['npm', 'run', 'build'], {}),
        ]
    report = {'schema_version': 1, 'kind': 'model_free_regression',
              'started_at': dt.datetime.now(dt.timezone.utc).isoformat(),
              'app_commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
              'source_sha256': before, 'formal_product_acceptance': 'not_evaluated',
              'real_devices_opened': False, 'model_inference': False,
              'two_machine_90_minute_rehearsal': 'not_evaluated', 'checks': []}
    report['acceptance_profile_sha256']=hashlib.sha256(acceptance_file.read_bytes()).hexdigest()
    report['unresolved_decisions']=[item['id'] for item in acceptance['open_decisions'] if item['status']=='open']
    for name, cwd, command, additions in commands:
        started = time.monotonic()
        log = args.output / f'{name}.log'
        with log.open('wb') as stream:
            try:
                result = subprocess.run(command, cwd=cwd, env=env | additions,
                                        stdout=stream, stderr=subprocess.STDOUT, timeout=900)
                code = result.returncode
            except (subprocess.TimeoutExpired, OSError) as error:
                stream.write(str(error).encode()); code = -1
        entry = {'name': name, 'command': command, 'exit_code': code,
                 'elapsed_seconds': round(time.monotonic() - started, 3),
                 'log': log.name, 'log_sha256': hashlib.sha256(log.read_bytes()).hexdigest()}
        report['checks'].append(entry)
        (args.output / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
        print(f'{name}: {"PASS" if code == 0 else "FAIL"} ({entry["elapsed_seconds"]}s)', flush=True)
    report['source_changed_during_run'] = before != source_hashes()
    report['regression_passed'] = all(c['exit_code'] == 0 for c in report['checks']) and not report['source_changed_during_run']
    (args.output / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    return 0 if report['regression_passed'] else 1

if __name__ == '__main__':
    raise SystemExit(main())
