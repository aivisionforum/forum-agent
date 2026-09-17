"""Explicit file-only real 8B probe; not part of automatic unit discovery.

No download or audio API. Output must be a new directory. This exercises the
production jobs.run process boundary with the source package and a specified
existing standalone Python. Timing is diagnostic, not meeting acceptance.
"""
import argparse
import json
import os
from pathlib import Path
import selectors
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'src'))
from forum_meeting_worker.fingerprint import model_fingerprint
from forum_meeting_worker.job_io import canonical, digest
from test_analysis import Fixture


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--python', required=True)
    parser.add_argument('--model', required=True)
    parser.add_argument('--output', required=True)
    args = parser.parse_args()
    output = Path(args.output).resolve()
    output.mkdir(mode=0o700)
    model = Path(args.model).resolve(strict=True)
    before = {str(p.relative_to(model)): [p.stat().st_size, p.stat().st_mtime_ns] for p in model.iterdir() if p.is_file()}
    f = Fixture(output, texts=[
        '主持人：今天没有批准公开发布。试点预算不是20万元，而是12万元。先评估两个方案，再作决定。',
        'Participant: We have not approved publication. The pilot budget is 120,000 yuan, not 200,000. We still need to compare two options.',
        '参会者：同意先试点，但目前没有确定负责人，也没有承诺完成日期。'])
    fingerprint_started = time.monotonic()
    fingerprint = model_fingerprint(model)
    fingerprint_seconds = time.monotonic() - fingerprint_started
    f.grant.update(profile='meeting-8b-v1', model_path=str(model), model_manifest_id=fingerprint)
    config = f.params['config']
    config.update(model_profile='meeting-8b-v1', model_manifest_id=fingerprint)
    config.pop('effective_config_hash')
    config['effective_config_hash'] = digest(canonical(config))
    f.snapshot['effective_config_hash'] = config['effective_config_hash']
    f.params.update(model_profile='meeting-8b-v1', remaining_budget_ms=90000)
    f.save()
    (output / 'run-request.json').write_bytes(canonical(f.params))
    (output / 'initialize.json').write_bytes(canonical(f.configuration))
    source = str(Path(__file__).resolve().parents[1] / 'src')
    code = f'import sys; sys.path.insert(0,{source!r}); from forum_meeting_worker.main import main; raise SystemExit(main())'
    stderr = (output / 'stderr.log').open('wb')
    process = subprocess.Popen([args.python, '-I', '-B', '-c', code], stdin=subprocess.PIPE,
        stdout=subprocess.PIPE, stderr=stderr, bufsize=0,
        env={'HOME': str(Path.home()), 'HF_HUB_OFFLINE': '1', 'TRANSFORMERS_OFFLINE': '1'})
    pending, events = bytearray(), []
    started = time.monotonic()
    def send(method, identity, params):
        data = canonical({'jsonrpc': '2.0', 'id': identity, 'method': method, 'params': params}) + b'\n'
        while data:
            data = data[process.stdin.write(data):]
    def read():
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            while b'\n' not in pending:
                if not selector.select(max(0, 100 - (time.monotonic() - started))):
                    raise TimeoutError('probe timeout')
                chunk = os.read(process.stdout.fileno(), 65536)
                if not chunk:
                    raise RuntimeError('worker EOF')
                pending.extend(chunk)
        index = pending.index(b'\n'); value = json.loads(pending[:index]); del pending[:index+1]
        events.append({'elapsed_seconds': time.monotonic() - started, 'message': value})
        return value
    try:
        send('initialize', 'init', f.configuration)
        assert 'result' in read()
        send('jobs.run', 'run', f.params)
        pinged = False
        while True:
            message = read()
            if message.get('method') == 'jobs.checkpoint':
                # This probe records the checkpoint, but cannot pretend a
                # database confirmation. No checkpoint ack is manufactured.
                pass
            if message.get('method') == 'jobs.progress' and not pinged:
                send('health.ping', 'during-compute', {}); pinged = True
            if message.get('id') == 'run':
                result = message
                break
        send('shutdown', 'shutdown', {})
        while read().get('id') != 'shutdown':
            pass
        exit_code = process.wait(timeout=5)
    finally:
        if process.poll() is None:
            process.terminate()
            process.wait(timeout=5)
        stderr.close()
    after = {str(p.relative_to(model)): [p.stat().st_size, p.stat().st_mtime_ns] for p in model.iterdir() if p.is_file()}
    report = {'model_manifest_id': fingerprint, 'fingerprint_seconds': fingerprint_seconds,
        'worker_seconds': time.monotonic() - started, 'exit_code': exit_code,
        'cache_unchanged': before == after, 'result': result,
        'attempt_directory': str(f.directory), 'synthetic_only': True,
        'claims_semantic_quality_not_certified': True}
    (output / 'events.jsonl').write_bytes(b''.join(canonical(e) + b'\n' for e in events))
    (output / 'report.json').write_bytes(canonical(report))
    print(json.dumps(report, ensure_ascii=False))
    return 0 if exit_code == 0 and 'result' in result and before == after else 1


if __name__ == '__main__':
    raise SystemExit(main())
