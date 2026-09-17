"""Explicit offline test of real ECAPA computation on a generated tone waveform.

This proves model loading/192-dimensional output and control responsiveness, not
speaker separation quality. No microphone, personal recording or model download.
"""
from __future__ import annotations
import argparse
import array
import hashlib
import json
import math
import os
from pathlib import Path
import selectors
import subprocess
import sys
import tempfile
import time
from uuid import uuid4


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--python', required=True)
    parser.add_argument('--model', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--installed', action='store_true', help='Import installed wheel instead of repository sources.')
    args = parser.parse_args()
    root = Path(tempfile.mkdtemp(prefix='forum-speaker-probe-'))
    model_path = args.model.resolve()
    checkpoint = model_path / 'embedding_model.ckpt'
    before = checkpoint.stat()
    manifest = 'sha256:' + hashlib.sha256(checkpoint.read_bytes()).hexdigest()
    job, session, track, segment = [str(uuid4()) for _ in range(4)]
    attempt = root / job / '1'; attempt.mkdir(parents=True, mode=0o700)
    samples = array.array('h', [int(7000 * math.sin(2 * math.pi * (140 + 15 * i / 48000) * i / 16000)
        + 2000 * math.sin(2 * math.pi * 420 * i / 16000)) for i in range(48000)])
    if sys.byteorder != 'little':
        samples.byteswap()
    pcm = samples.tobytes(); (attempt / 'segment.pcm').write_bytes(pcm)
    os.chmod(attempt / 'segment.pcm', 0o600)
    config = {'protocol_version': 1, 'instance_id': str(uuid4()), 'job_root': str(root),
        'model_grant': {'profile': 'ecapa-voxceleb-v1', 'model_path': str(model_path), 'model_manifest_id': manifest}}
    params = {'job_id': job, 'attempt': 1, 'remaining_budget_ms': 120000, 'session_id': session,
        'track_id': track, 'segment_id': segment, 'segment_revision': 1, 'overlap': False,
        'pcm': {'relative_path': 'segment.pcm', 'sha256': hashlib.sha256(pcm).hexdigest(), 'sample_rate': 16000, 'channels': 1, 'format': 's16le'}}
    source = Path(__file__).resolve().parents[1] / 'src'
    command = [args.python, '-I', '-B', '-m', 'forum_speaker_worker'] if args.installed else [args.python, '-I', '-B', '-c', f'import sys;sys.path.insert(0,{str(source)!r});from forum_speaker_worker.main import main;raise SystemExit(main())']
    report = {'fixture': 'synthetic_tones_not_speaker_accuracy', 'formal_C3_acceptance': False,
        'pcm_seconds': 3, 'model_manifest_id': manifest, 'model_path': str(model_path), 'python': args.python,
        'installed_module': args.installed, 'job_directory': str(attempt)}
    log = root / 'stderr.log'
    started = time.monotonic()
    child = None
    pending = bytearray()
    try:
        with log.open('wb') as stderr:
            child = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr, start_new_session=True, bufsize=0)
            def send(identifier, method, payload):
                child.stdin.write(json.dumps({'jsonrpc': '2.0', 'id': identifier, 'method': method, 'params': payload}).encode() + b'\n')
                child.stdin.flush()
            def read():
                with selectors.DefaultSelector() as selector:
                    selector.register(child.stdout, selectors.EVENT_READ)
                    while time.monotonic() - started < 125:
                        if (index := pending.find(b'\n')) >= 0:
                            message = json.loads(pending[:index]); del pending[:index + 1]
                            return message
                        if selector.select(0.1):
                            data = os.read(child.stdout.fileno(), 16384)
                            if not data:
                                raise RuntimeError('Worker EOF without response')
                            pending.extend(data)
                    raise RuntimeError('Probe deadline expired')
            send(1, 'initialize', config); report['initialize'] = read()
            if 'error' in report['initialize']:
                raise RuntimeError('Initialize rejected')
            send(2, 'jobs.run', params)
            time.sleep(0.1)
            ping_started = time.monotonic(); send(3, 'health.ping', {})
            responses = []
            for _ in range(2):
                response = read(); responses.append(response)
                if response['id'] == 3:
                    report['ping_latency_seconds'] = time.monotonic() - ping_started
                if response['id'] == 2:
                    report['analysis_response'] = response
            report['elapsed_seconds'] = time.monotonic() - started
            send(4, 'shutdown', {}); report['shutdown'] = read()
            child.wait(timeout=3)
            report['exit_code'] = child.returncode
            result = report['analysis_response'].get('result')
            if result is None or result['status'] != 'embedding':
                raise RuntimeError('Real model did not return an embedding')
            norm = math.sqrt(sum(x * x for x in result['embedding']))
            if len(result['embedding']) != 192 or not math.isfinite(norm) or abs(norm - 1) > 1e-5:
                raise RuntimeError('Embedding normalization failed')
            report['embedding_dimension'] = len(result['embedding'])
            report['embedding_l2_norm'] = norm
            report['success'] = True
    except Exception as exc:
        report['success'] = False
        report['error'] = str(exc)
    finally:
        if child is not None:
            if child.poll() is None:
                # This probe creates and owns this process group exclusively.
                os.killpg(child.pid, 9); child.wait(timeout=3)
            for pipe in (child.stdin, child.stdout):
                if pipe and not pipe.closed:
                    pipe.close()
        after = checkpoint.stat()
        report['model_cache_unchanged'] = (before.st_size, before.st_mtime_ns) == (after.st_size, after.st_mtime_ns)
        report['stderr_log'] = str(log)
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
        print(json.dumps({key: report.get(key) for key in ('success', 'elapsed_seconds', 'ping_latency_seconds', 'embedding_dimension', 'model_cache_unchanged', 'error')}))
    return 0 if report['success'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
