"""Responsive control plane; one host-granted immutable attempt per worker."""
from __future__ import annotations

import os
from pathlib import Path
import selectors
import subprocess
import sys
import threading
import time

from .analysis import validate_run
from .job_io import JobError, canonical, integer, require, strict_json, uuid


class Jobs:
    def __init__(self, emit, *, allow_fake=False):
        self.emit, self.allow_fake = emit, allow_fake
        self.lock = threading.Lock()
        self.thread = None
        self.process = None
        self.identity = None
        self.cancelled = threading.Event()
        self.paused = False

    def start(self, request_id, params, configuration):
        validate_run(params, configuration)
        with self.lock:
            require(self.identity is None, 'This worker already owns an immutable attempt; spawn a new worker.', 'RESOURCE_BUSY')
            self.identity = (params['job_id'], params['attempt'])
            deadline = time.monotonic() + params['remaining_budget_ms'] / 1000
            self.thread = threading.Thread(target=self._execute,
                args=(request_id, params, configuration, deadline), name='analysis-control', daemon=False)
            self.thread.start()

    def cancel(self, params):
        require(isinstance(params, dict) and set(params) == {'job_id', 'attempt'}, 'Cancel requires job_id and attempt.')
        uuid(params['job_id']); integer(params['attempt'], 1, 1000)
        with self.lock:
            require(self.identity == (params['job_id'], params['attempt']), 'Cancel does not identify the owned attempt.', 'INVALID_PARAMS')
            self.cancelled.set()
            self._signal_cancel()
        return {'job_id': params['job_id'], 'attempt': params['attempt'], 'status': 'cancel_requested'}

    def _signal_cancel(self):
        self._signal(b'!')

    def set_paused(self, params):
        require(isinstance(params, dict) and set(params) == {'job_id', 'attempt', 'paused'}, 'Flow control requires job_id, attempt and paused.')
        uuid(params['job_id']); integer(params['attempt'], 1, 1000)
        require(type(params['paused']) is bool, 'paused must be a boolean.')
        with self.lock:
            require(self.identity == (params['job_id'], params['attempt']), 'Flow control identifies another attempt.')
            self.paused = params['paused']
            self._signal(b'P' if self.paused else b'R')
        return {'job_id': params['job_id'], 'attempt': params['attempt'], 'status': 'flow_requested'}

    def _signal(self, command):
        if self.process is not None and self.process.stdin:
            try:
                self.process.stdin.write(command)
                self.process.stdin.flush()
            except (BrokenPipeError, OSError):
                pass

    def close(self):
        with self.lock:
            self.cancelled.set()
            self._signal_cancel()
        if self.thread:
            self.thread.join(2)
        # A native uninterruptible call is still owned. The non-daemon thread
        # keeps this process alive so the host's 2 s PGID supervisor must reap
        # it; returning a shutdown ack never asserts actual compute exit.

    def _execute(self, request_id, params, configuration, deadline):
        process, final = None, None
        failure = None
        try:
            environment = {key: os.environ[key] for key in ('HOME', 'TMPDIR', 'LANG') if key in os.environ}
            environment.update(HF_HUB_OFFLINE='1', TRANSFORMERS_OFFLINE='1',
                               HF_HUB_DISABLE_TELEMETRY='1', HF_HUB_DISABLE_IMPLICIT_TOKEN='1')
            process = subprocess.Popen([sys.executable, '-I', '-B', str(Path(__file__).with_name('_compute.py'))],
                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=None, env=environment,
                cwd='/', bufsize=0, close_fds=True)
            envelope = {'params': params, 'configuration': configuration, 'allow_fake': self.allow_fake,
                        'deadline_monotonic': deadline}
            data = canonical(envelope) + b'\n'
            require(len(data) <= 1024 * 1024, 'Compute initialization frame too large.')
            while data:
                data = data[process.stdin.write(data):]
            with self.lock:
                self.process = process
                if self.cancelled.is_set():
                    self._signal_cancel()
                elif self.paused:
                    self._signal(b'P')
            pending = bytearray()
            with selectors.DefaultSelector() as selector:
                selector.register(process.stdout, selectors.EVENT_READ)
                while True:
                    if self.cancelled.is_set():
                        raise JobError('CANCELLED', 'Analysis was cancelled.')
                    if time.monotonic() >= deadline:
                        raise JobError('DEADLINE_EXCEEDED', 'Analysis total budget expired.')
                    if not selector.select(min(0.05, max(0, deadline - time.monotonic()))):
                        continue
                    chunk = os.read(process.stdout.fileno(), 65536)
                    if not chunk:
                        require(not pending and final is not None, 'Compute exited without a complete result.', 'WORKER_EXITED')
                        break
                    pending.extend(chunk)
                    while (index := pending.find(b'\n')) >= 0:
                        require(index <= 1024 * 1024, 'Compute frame too large.', 'INVALID_MODEL_OUTPUT')
                        value = strict_json(bytes(pending[:index])); del pending[:index + 1]
                        require(isinstance(value, dict) and set(value) == {'method', 'params'}, 'Invalid compute protocol.', 'WORKER_EXITED')
                        method = value['method']
                        if method in ('result', 'error'):
                            require(final is None, 'Duplicate compute result.', 'WORKER_EXITED')
                            final = value
                        else:
                            require(method in ('jobs.progress', 'jobs.checkpoint', 'jobs.flow') and final is None, 'Invalid compute notification.', 'WORKER_EXITED')
                            self.emit({'jsonrpc': '2.0', **value})
                    require(len(pending) <= 1024 * 1024, 'Compute frame too large.', 'INVALID_MODEL_OUTPUT')
            require(process.wait(timeout=max(0.01, deadline - time.monotonic())) == 0,
                    'Compute process exited unsuccessfully.', 'WORKER_EXITED')
        except JobError as exc:
            failure = {'code': exc.code, 'message': str(exc)}
        except Exception as exc:
            failure = {'code': 'WORKER_EXITED', 'message': 'Owned compute or storage failed: ' + type(exc).__name__}
        finally:
            if process:
                if process.poll() is None:
                    self._signal_cancel()
                    try:
                        process.wait(timeout=0.5)
                    except subprocess.TimeoutExpired:
                        # Only this exact Popen child. Group-wide containment
                        # is exclusively the Rust host's responsibility.
                        process.terminate()
                        try:
                            process.wait(timeout=0.5)
                        except subprocess.TimeoutExpired:
                            process.kill()
                            process.wait()
                if process.stdin:
                    process.stdin.close()
                if process.stdout:
                    process.stdout.close()
            with self.lock:
                self.process = None
            if self.cancelled.is_set():
                failure = {'code': 'CANCELLED', 'message': 'Analysis was cancelled.'}
            if failure is None and final and final['method'] == 'error':
                failure = final['params']
            response = {'jsonrpc': '2.0', 'id': request_id}
            if failure:
                response['error'] = {'code': -32010, 'message': failure['message'], 'data': {'code': failure['code']}}
            else:
                response['result'] = final['params']
            try:
                self.emit(response)
            except (BrokenPipeError, OSError):
                pass
