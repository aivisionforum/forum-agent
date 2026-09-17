"""One owned native inference child. Inherits the Rust supervisor process group."""
from __future__ import annotations
import json
import sys
from pathlib import Path

# Called through an absolute installed/source path under -I; no cwd imports.
if __package__ in (None, ''):
    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from forum_speaker_worker.input import WorkerError, audio_rejection, canonical, load_pcm, result_base, strict_json


def main():
    try:
        line = sys.stdin.buffer.readline(65537)
        if len(line) > 65536 or not line.endswith(b'\n'):
            raise WorkerError('INVALID_PARAMS', 'Compute envelope is not bounded JSON.')
        value = strict_json(line)
        configuration, params = value['configuration'], value['params']
        samples, quality = load_pcm(configuration, params)
        result = result_base(configuration, params, quality)
        rejection = audio_rejection(params, quality)
        if rejection:
            result['status'], result['reason'] = rejection
        else:
            # Third-party imports may print; only the final envelope goes to stdout.
            original = sys.stdout
            sys.stdout = sys.stderr
            try:
                from forum_speaker_worker.model import embed
                result['embedding'] = embed(samples, configuration['model_grant'])
            finally:
                sys.stdout = original
            result['status'] = 'embedding'
        response = {'result': result}
    except WorkerError as exc:
        response = {'error': {'code': exc.code, 'message': str(exc)}}
    except Exception as exc:
        response = {'error': {'code': 'WORKER_FAILED', 'message': 'Speaker inference failed: ' + type(exc).__name__}}
    sys.stdout.buffer.write(canonical(response) + b'\n')
    sys.stdout.buffer.flush()
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
