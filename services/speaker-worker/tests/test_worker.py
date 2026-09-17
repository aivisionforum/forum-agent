from __future__ import annotations
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
import unittest
from uuid import uuid4

from forum_speaker_worker.input import WorkerError, audio_rejection, canonical, load_pcm, validate_initialize, validate_run
from forum_speaker_worker.main import Protocol
from forum_speaker_worker.model import checkpoint_bytes

SRC = Path(__file__).resolve().parents[1] / 'src'


class Fixture(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.model = self.root / 'model'
        self.model.mkdir()
        self.checkpoint = self.model / 'embedding_model.ckpt'
        self.checkpoint.write_bytes(b'checkpoint-test')
        self.configuration = {'protocol_version': 1, 'instance_id': str(uuid4()), 'job_root': str(self.root),
            'model_grant': {'profile': 'ecapa-voxceleb-v1', 'model_path': str(self.model),
                'model_manifest_id': 'sha256:' + hashlib.sha256(self.checkpoint.read_bytes()).hexdigest()}}
        self.params = {'job_id': str(uuid4()), 'attempt': 1, 'remaining_budget_ms': 10000,
            'session_id': str(uuid4()), 'track_id': str(uuid4()), 'segment_id': str(uuid4()), 'segment_revision': 1,
            'pcm': {'relative_path': 'segment.pcm', 'sha256': '', 'sample_rate': 16000, 'channels': 1, 'format': 's16le'}, 'overlap': False}
        self.directory = self.root / self.params['job_id'] / '1'
        self.directory.mkdir(parents=True)
        self.pcm = self.directory / 'segment.pcm'
        self.audio([0] * 32000)

    def tearDown(self):
        self.tmp.cleanup()

    def audio(self, samples):
        data = array.array('h', samples)
        if sys.byteorder != 'little':
            data.byteswap()
        self.pcm.write_bytes(data.tobytes())
        self.params['pcm']['sha256'] = hashlib.sha256(self.pcm.read_bytes()).hexdigest()

    def command(self):
        return [sys.executable, '-I', '-B', '-c', f'import sys;sys.path.insert(0,{str(SRC)!r});from forum_speaker_worker.main import main;raise SystemExit(main())']

    def worker(self, *extra):
        child = subprocess.Popen(self.command() + list(extra), stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)
        self.addCleanup(self.stop, child)
        return child

    def stop(self, child):
        if child.poll() is None:
            child.stdin.close()
            try:
                child.wait(timeout=3)
            except subprocess.TimeoutExpired:
                child.kill(); child.wait(timeout=3)
        for pipe in (child.stdin, child.stdout, child.stderr):
            if not pipe.closed:
                pipe.close()

    def send(self, child, request_id, method, params=None):
        child.stdin.write(canonical({'jsonrpc': '2.0', 'id': request_id, 'method': method, 'params': params or {}}) + b'\n')
        child.stdin.flush()

    def read(self, child, timeout=5):
        deadline = time.monotonic() + timeout
        data = bytearray()
        with selectors.DefaultSelector() as selector:
            selector.register(child.stdout, selectors.EVENT_READ)
            while time.monotonic() < deadline:
                if selector.select(0.05):
                    chunk = os.read(child.stdout.fileno(), 1)
                    if not chunk:
                        self.fail('Worker EOF: ' + child.stderr.read().decode())
                    data.extend(chunk)
                    if chunk == b'\n':
                        return json.loads(data)
        self.fail('Worker response timed out')

    def init(self, child):
        self.send(child, 1, 'initialize', self.configuration)
        self.assertIn('result', self.read(child))


class ValidationTests(Fixture):
    def test_pcm_hash_and_metadata(self):
        self.assertEqual(validate_run(self.params), self.params)
        samples, quality = load_pcm(self.configuration, self.params)
        self.assertEqual(len(samples), 32000)
        self.assertEqual(quality, {'duration_seconds': 2.0, 'rms': 0.0, 'clipping_ratio': 0.0})
        self.params['pcm']['sha256'] = 'f' * 64
        with self.assertRaises(WorkerError):
            load_pcm(self.configuration, self.params)

    def test_symlink_and_fifo_do_not_open(self):
        target = self.root / 'other.pcm'
        self.pcm.rename(target)
        self.pcm.symlink_to(target)
        with self.assertRaises(WorkerError):
            load_pcm(self.configuration, self.params)
        self.pcm.unlink(); os.mkfifo(self.pcm)
        start = time.monotonic()
        with self.assertRaises(WorkerError):
            load_pcm(self.configuration, self.params)
        self.assertLess(time.monotonic() - start, 0.2)

    def test_symlink_attempt_directory_rejected(self):
        original = self.directory.with_name('other')
        self.directory.rename(original)
        self.directory.symlink_to(original, target_is_directory=True)
        with self.assertRaises(WorkerError):
            load_pcm(self.configuration, self.params)

    def test_short_silence_clipping_and_overlap_never_assign(self):
        quality = {'duration_seconds': 0.8, 'rms': 0.5, 'clipping_ratio': 0}
        self.assertEqual(audio_rejection(self.params, quality), ('unknown', 'too_short'))
        quality.update(duration_seconds=2, rms=0)
        self.assertEqual(audio_rejection(self.params, quality), ('unknown', 'quiet_or_silent'))
        quality.update(rms=0.4, clipping_ratio=0.03)
        self.assertEqual(audio_rejection(self.params, quality), ('unknown', 'clipped_audio'))
        self.params['overlap'] = True
        self.assertEqual(audio_rejection(self.params, quality), ('overlap', 'host_reported_overlap'))

    def test_audio_size_limits(self):
        for size in (0, 3, 16000 * 2 * 30 + 2):
            self.pcm.write_bytes(b'\0' * size)
            with self.assertRaises(WorkerError):
                load_pcm(self.configuration, self.params)

    def test_no_request_overrides_model_or_path(self):
        for key, value in [('model_path', '/tmp'), ('segment_revision', True), ('remaining_budget_ms', 120001)]:
            params = dict(self.params, **{key: value})
            with self.assertRaises(WorkerError):
                validate_run(params)
        self.params['pcm']['relative_path'] = '../segment.pcm'
        with self.assertRaises(WorkerError):
            validate_run(self.params)

    def test_checkpoint_byte_grant_only_and_no_yaml(self):
        (self.model / 'hyperparams.yaml').write_text('!!python/object/apply:os.system ["echo never"]')
        self.assertEqual(checkpoint_bytes(self.configuration['model_grant']), b'checkpoint-test')
        self.checkpoint.write_bytes(b'changed')
        with self.assertRaisesRegex(WorkerError, 'hash differs'):
            checkpoint_bytes(self.configuration['model_grant'])

    def test_checkpoint_fifo_bounded(self):
        self.checkpoint.unlink(); os.mkfifo(self.checkpoint)
        start = time.monotonic()
        with self.assertRaises(WorkerError):
            checkpoint_bytes(self.configuration['model_grant'])
        self.assertLess(time.monotonic() - start, 0.2)

    def test_missing_model_is_explicit(self):
        self.configuration['model_grant']['model_path'] = str(self.root / 'missing')
        with self.assertRaises(WorkerError) as caught:
            validate_initialize(self.configuration)
        self.assertEqual(caught.exception.code, 'MODEL_UNAVAILABLE')


class ProtocolTests(Fixture):
    def test_initialize_ping_shutdown_without_model_import(self):
        child = self.worker(); self.init(child)
        self.send(child, 2, 'health.ping')
        self.assertTrue(self.read(child)['result']['initialized'])
        self.send(child, 3, 'shutdown')
        self.assertEqual(self.read(child)['result']['status'], 'shutdown_requested')
        self.assertEqual(child.wait(timeout=3), 0)

    def test_invalid_and_duplicate_json(self):
        protocol = Protocol(lambda response: None)
        self.assertEqual(protocol.handle(b'{"jsonrpc":"2.0","id":1,"id":2}')['error']['data']['code'], 'PARSE_ERROR')
        self.assertEqual(protocol.handle(b'[]')['error']['data']['code'], 'INVALID_REQUEST')
        self.assertEqual(protocol.handle(b'{"jsonrpc":"2.0","method":"health.ping","id":true}')['error']['data']['code'], 'INVALID_REQUEST')

    def test_quiet_audio_response_identity_and_one_attempt(self):
        child = self.worker(); self.init(child)
        self.send(child, 2, 'jobs.run', self.params)
        result = self.read(child)['result']
        for key in ('session_id', 'track_id', 'segment_id', 'segment_revision'):
            self.assertEqual(result[key], self.params[key])
        self.assertEqual(result['status'], 'unknown')
        self.assertEqual(result['reason'], 'quiet_or_silent')
        self.assertIsNone(result['embedding'])
        self.send(child, 3, 'jobs.run', self.params)
        self.assertEqual(self.read(child)['error']['data']['code'], 'RESOURCE_BUSY')

    def test_cancel_is_responsive_and_reaps_compute(self):
        self.audio([int(10000 * math.sin(2 * math.pi * 330 * i / 16000)) for i in range(32000)])
        child = self.worker(); self.init(child)
        started = time.monotonic()
        self.send(child, 2, 'jobs.run', self.params)
        self.send(child, 3, 'health.ping')
        self.send(child, 4, 'jobs.cancel', {key: self.params[key] for key in ('job_id', 'attempt')})
        responses = {response['id']: response for response in (self.read(child), self.read(child), self.read(child))}
        self.assertEqual(responses[3]['result']['status'], 'ok')
        self.assertEqual(responses[4]['result']['status'], 'cancel_requested')
        self.assertEqual(responses[2]['error']['data']['code'], 'CANCELLED')
        self.assertLess(time.monotonic() - started, 2)
        self.send(child, 5, 'shutdown'); self.read(child)
        self.assertEqual(child.wait(timeout=2), 0)

    def test_deadline_covers_startup(self):
        child = self.worker(); self.init(child)
        self.params['remaining_budget_ms'] = 1
        self.send(child, 2, 'jobs.run', self.params)
        self.assertEqual(self.read(child)['error']['data']['code'], 'DEADLINE_EXCEEDED')

    def test_bad_cancel_identity_cannot_cancel_other_attempt(self):
        protocol = Protocol(lambda response: None)
        request = {'jsonrpc': '2.0', 'id': 1, 'method': 'jobs.cancel', 'params': {'job_id': str(uuid4()), 'attempt': 1}}
        self.assertEqual(protocol.handle(canonical(request))['error']['data']['code'], 'INVALID_PARAMS')

    def test_incomplete_frame_timeout(self):
        child = self.worker('--frame-timeout-seconds', '0.1')
        child.stdin.write(b'{'); child.stdin.flush()
        self.assertEqual(self.read(child)['error']['data']['code'], 'FRAME_TIMEOUT')
        self.assertEqual(child.wait(timeout=2), 2)

    def test_oversized_frame_closes_transport(self):
        child = self.worker()
        child.stdin.write(b' ' * 65537 + b'\n'); child.stdin.flush()
        self.assertEqual(self.read(child)['error']['data']['code'], 'FRAME_TOO_LARGE')
        self.assertEqual(child.wait(timeout=2), 2)


if __name__ == '__main__':
    unittest.main()
