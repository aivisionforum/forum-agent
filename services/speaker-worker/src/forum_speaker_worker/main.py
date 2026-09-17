"""Bounded NDJSON control remains responsive while ECAPA executes in a child."""
from __future__ import annotations

import argparse
import json
import math
import os
from pathlib import Path
import selectors
import subprocess
import sys
import threading
import time

from . import __version__
from .input import WorkerError, canonical, fields, integer, require, strict_json, uuid, validate_initialize, validate_run

MAX_FRAME_BYTES = 65536


def frames(stream, timeout):
    pending, started = bytearray(), None
    with selectors.DefaultSelector() as selector:
        selector.register(stream, selectors.EVENT_READ)
        while True:
            remaining = None if started is None else timeout - (time.monotonic() - started)
            if remaining is not None and remaining <= 0 or not selector.select(remaining):
                raise WorkerError('FRAME_TIMEOUT', 'Incomplete frame timed out.')
            data = os.read(stream.fileno(), 65536)
            if not data:
                require(not pending, 'Expected newline before EOF.', 'INCOMPLETE_FRAME')
                return
            if started is None:
                started = time.monotonic()
            pending.extend(data)
            while (index := pending.find(b'\n')) >= 0:
                require(index <= MAX_FRAME_BYTES, 'Frame exceeds 64 KiB.', 'FRAME_TOO_LARGE')
                frame = bytes(pending[:index])
                del pending[:index + 1]
                started = time.monotonic() if pending else None
                yield frame
            require(len(pending) <= MAX_FRAME_BYTES, 'Frame exceeds 64 KiB.', 'FRAME_TOO_LARGE')


def error(request_id, code, message, number=-32010):
    return {'jsonrpc': '2.0', 'id': request_id, 'error': {'code': number, 'message': message, 'data': {'code': code}}}


class Jobs:
    def __init__(self, emit):
        self.emit = emit
        self.identity = None
        self.thread = None
        self.cancelled = threading.Event()

    def start(self, request_id, configuration, params):
        validate_run(params)
        require(self.identity is None, 'This worker already owns an attempt; spawn a new worker.', 'RESOURCE_BUSY')
        envelope = canonical({'configuration': configuration, 'params': params}) + b'\n'
        require(len(envelope) <= 16384, 'Compute configuration is too large.')
        self.identity = (params['job_id'], params['attempt'])
        deadline = time.monotonic() + params['remaining_budget_ms'] / 1000
        self.thread = threading.Thread(target=self._run, args=(request_id, envelope, deadline), name='speaker-control', daemon=False)
        self.thread.start()

    def cancel(self, params):
        fields(params, 'job_id attempt')
        uuid(params['job_id']); integer(params['attempt'], 1, 1000)
        require(self.identity == (params['job_id'], params['attempt']), 'Cancel does not identify the owned attempt.')
        self.cancelled.set()
        return {**params, 'status': 'cancel_requested'}

    def close(self):
        self.cancelled.set()
        if self.thread:
            self.thread.join(2)
        # An uninterruptible native child cannot be declared stopped by an ack.
        # This non-daemon thread stays owned until Rust reaps the process group.

    def _check(self, deadline):
        require(not self.cancelled.is_set(), 'Speaker inference cancelled.', 'CANCELLED')
        require(time.monotonic() < deadline, 'Speaker inference deadline expired.', 'DEADLINE_EXCEEDED')

    def _run(self, request_id, envelope, deadline):
        child, response = None, None
        try:
            env = {key: os.environ[key] for key in ('HOME', 'TMPDIR', 'LANG') if key in os.environ}
            env.update(HF_HUB_OFFLINE='1', TRANSFORMERS_OFFLINE='1', HF_HUB_DISABLE_TELEMETRY='1', HF_HUB_DISABLE_IMPLICIT_TOKEN='1', OMP_NUM_THREADS='2', MKL_NUM_THREADS='2')
            child = subprocess.Popen([sys.executable, '-I', '-B', str(Path(__file__).with_name('_compute.py'))],
                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=None, env=env, cwd='/', close_fds=True, bufsize=0)
            # No setsid / start_new_session: the entire tree belongs to Rust.
            os.set_blocking(child.stdin.fileno(), False)
            with selectors.DefaultSelector() as selector:
                selector.register(child.stdin, selectors.EVENT_WRITE)
                while envelope:
                    self._check(deadline)
                    if selector.select(0.05):
                        try:
                            written = os.write(child.stdin.fileno(), envelope)
                            envelope = envelope[written:]
                        except BlockingIOError:
                            pass
            child.stdin.close()
            pending = bytearray()
            with selectors.DefaultSelector() as selector:
                selector.register(child.stdout, selectors.EVENT_READ)
                while True:
                    self._check(deadline)
                    if not selector.select(0.05):
                        continue
                    chunk = os.read(child.stdout.fileno(), 16384)
                    if not chunk:
                        require(pending.endswith(b'\n') and pending.count(b'\n') == 1, 'Compute exited without exactly one response.', 'WORKER_EXITED')
                        response = strict_json(pending)
                        require(isinstance(response, dict) and (set(response) == {'result'} or set(response) == {'error'}), 'Malformed compute response.', 'WORKER_EXITED')
                        break
                    pending.extend(chunk)
                    require(len(pending) <= MAX_FRAME_BYTES, 'Compute response exceeded 64 KiB.', 'WORKER_EXITED')
            self._check(deadline)
            require(child.wait(timeout=max(0.01, deadline - time.monotonic())) == 0, 'Compute exited unsuccessfully.', 'WORKER_EXITED')
        except WorkerError as exc:
            response = {'error': {'code': exc.code, 'message': str(exc)}}
        except Exception as exc:
            response = {'error': {'code': 'WORKER_EXITED', 'message': 'Owned speaker process failed: ' + type(exc).__name__}}
        finally:
            if child:
                if child.poll() is None:
                    child.terminate()
                    try:
                        child.wait(timeout=0.5)
                    except subprocess.TimeoutExpired:
                        child.kill()
                        # If native cleanup does not return, Rust's independent
                        # process-group deadline remains the containment boundary.
                        child.wait()
                if child.stdin and not child.stdin.closed:
                    child.stdin.close()
                child.stdout.close()
            if self.cancelled.is_set():
                response = {'error': {'code': 'CANCELLED', 'message': 'Speaker inference cancelled.'}}
            if response is None:
                response = {'error': {'code': 'WORKER_EXITED', 'message': 'Speaker process returned no result.'}}
            message = {'jsonrpc': '2.0', 'id': request_id}
            if 'error' in response:
                message = error(request_id, response['error']['code'], response['error']['message'])
            else:
                message['result'] = response['result']
            try:
                self.emit(message)
            except (BrokenPipeError, OSError):
                pass


class Protocol:
    def __init__(self, emit):
        self.configuration = None
        self.stopping = False
        self.jobs = Jobs(emit)

    def handle(self, frame):
        try:
            request = strict_json(frame)
        except (UnicodeDecodeError, ValueError, RecursionError):
            return error(None, 'PARSE_ERROR', 'Invalid UTF-8 JSON.', -32700)
        if not isinstance(request, dict) or set(request) - {'jsonrpc', 'id', 'method', 'params'} or request.get('jsonrpc') != '2.0' or not isinstance(request.get('method'), str):
            return error(None, 'INVALID_REQUEST', 'Expected JSON-RPC object.', -32600)
        request_id = request.get('id')
        if request_id is not None and not isinstance(request_id, str) and type(request_id) is not int:
            return error(None, 'INVALID_REQUEST', 'Invalid request ID.', -32600)
        method, params = request['method'], request.get('params', {})
        try:
            if method == 'initialize':
                require(self.configuration is None, 'Already initialized.', 'ALREADY_INITIALIZED')
                self.configuration = validate_initialize(params)
                result = {'protocol_version': 1, 'worker_version': __version__, 'capabilities': {'health': True, 'task_types': ['speaker_embedding'], 'model_clients': ['local-ecapa']}}
            elif method == 'health.ping':
                result = {'status': 'ok', 'initialized': self.configuration is not None}
            elif method == 'shutdown':
                self.stopping = True
                self.jobs.cancelled.set()
                result = {'status': 'shutdown_requested'}
            elif method == 'jobs.run':
                if 'id' not in request:
                    return None
                require(self.configuration is not None, 'Initialize before inference.', 'NOT_INITIALIZED')
                self.jobs.start(request_id, self.configuration, params)
                return None
            elif method == 'jobs.cancel':
                result = self.jobs.cancel(params)
            else:
                raise WorkerError('METHOD_NOT_FOUND', 'Method is not supported.')
            return {'jsonrpc': '2.0', 'id': request_id, 'result': result} if 'id' in request else None
        except WorkerError as exc:
            return error(request_id, exc.code, str(exc)) if 'id' in request else None


def main():
    parser = argparse.ArgumentParser(description='Optional local speaker embedding worker')
    parser.add_argument('--frame-timeout-seconds', type=float, default=10)
    args = parser.parse_args()
    if not math.isfinite(args.frame_timeout_seconds) or args.frame_timeout_seconds <= 0:
        parser.error('frame timeout must be finite and positive')
    lock = threading.Lock()
    def emit(message):
        with lock:
            sys.stdout.buffer.write(canonical(message) + b'\n')
            sys.stdout.buffer.flush()
    protocol = Protocol(emit)
    try:
        for frame in frames(sys.stdin.buffer, args.frame_timeout_seconds):
            response = protocol.handle(frame)
            if response is not None:
                emit(response)
            if protocol.stopping:
                break
        return 0
    except WorkerError as exc:
        emit(error(None, exc.code, str(exc)))
        return 2
    except (BrokenPipeError, OSError):
        return 1
    finally:
        protocol.jobs.close()
