"""Private subprocess entry, never an independently exposed RPC service.

Launched with Python -I -B and an absolute packaged script path. No setsid:
the host-owned process group contains control and compute processes together.
"""
from pathlib import Path
import json
import os
import sys
import threading
import time

# Only the installed/trusted package parent, not cwd or an environment path.
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from forum_meeting_worker.analysis import execute
from forum_meeting_worker.flow import ComputeFlow
from forum_meeting_worker.job_io import JobError, canonical, strict_json
from forum_meeting_worker.model_probe import library_stdout_to_stderr


def main():
    pending = bytearray()
    while b'\n' not in pending:
        chunk = os.read(0, 65536)
        if not chunk or len(pending) + len(chunk) > 1024 * 1024 + 1:
            return 2
        pending.extend(chunk)
    line, _, extra = pending.partition(b'\n')
    envelope = strict_json(line)
    deadline = envelope['deadline_monotonic']
    protocol_fd = os.dup(1)
    def emit(method, params):
        data = canonical({'method': method, 'params': params}) + b'\n'
        if len(data) > 1024 * 1024:
            raise JobError('INVALID_MODEL_OUTPUT', 'Child protocol frame exceeded limit.')
        while data:
            data = data[os.write(protocol_fd, data):]
    def control():
        # Private P/R/! flow protocol. EOF cancels even while paused.
        # Never hold BufferedReader's lock in a daemon thread: CPython would
        # abort at interpreter shutdown while this read waits for input.
        while True:
            command = os.read(0, 64)
            flow.receive(command)
            if not command:
                return
    identity = {key: envelope['params'][key] for key in ('job_id', 'attempt')}
    flow = ComputeFlow(deadline, lambda paused: emit('jobs.flow', identity | {'paused': paused}))
    if extra:
        flow.receive(extra)
    threading.Thread(target=control, daemon=True).start()
    check = flow.check
    try:
        with library_stdout_to_stderr():
            result = execute(envelope['params'], envelope['configuration'], check, emit,
                             envelope['allow_fake'])
        check()
        emit('result', result)
    except JobError as exc:
        emit('error', {'code': exc.code, 'message': str(exc)})
    except OSError as exc:
        print('meeting storage failed: ' + type(exc).__name__, file=sys.stderr)
        emit('error', {'code': 'STORAGE_UNAVAILABLE', 'message': 'Attempt storage failed.'})
    except BaseException as exc:
        print('meeting computation failed: ' + type(exc).__name__, file=sys.stderr)
        emit('error', {'code': 'WORKER_EXITED', 'message': 'Analysis compute failed; see stderr error type.'})
    finally:
        os.close(protocol_fd)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
