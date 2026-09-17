"""Validate host grants and immutable PCM without importing ML dependencies."""
from __future__ import annotations

import array
import hashlib
import json
import math
import os
from pathlib import Path
import stat
import sys
from uuid import UUID

MAX_PCM_BYTES = 16000 * 2 * 30
PROFILE = 'ecapa-voxceleb-v1'


class WorkerError(Exception):
    def __init__(self, code: str, message: str):
        super().__init__(message)
        self.code = code


def require(condition, message, code='INVALID_PARAMS'):
    if not condition:
        raise WorkerError(code, message)


def fields(value, names):
    require(isinstance(value, dict) and set(value) == set(names.split()), 'Unexpected or missing fields.')


def integer(value, lower, upper):
    require(type(value) is int and lower <= value <= upper, 'Integer outside allowed range.')


def uuid(value):
    require(isinstance(value, str), 'Identity must be a UUID string.')
    try:
        require(str(UUID(value)) == value, 'Identity must be a canonical UUID.')
    except (ValueError, AttributeError):
        raise WorkerError('INVALID_PARAMS', 'Identity must be a canonical UUID.') from None


def sha(value):
    require(isinstance(value, str) and len(value) == 64 and all(c in '0123456789abcdef' for c in value), 'Invalid SHA-256.')


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False, allow_nan=False).encode('utf8')


def strict_json(data):
    def pairs(items):
        out = {}
        for key, value in items:
            if key in out:
                raise ValueError('Duplicate field')
            out[key] = value
        return out
    def constant(_):
        raise ValueError('Non-finite JSON')
    return json.loads(data, object_pairs_hook=pairs, parse_constant=constant)


def validate_initialize(value):
    fields(value, 'protocol_version instance_id job_root model_grant')
    integer(value['protocol_version'], 1, 1)
    uuid(value['instance_id'])
    require(isinstance(value['job_root'], str) and Path(value['job_root']).is_absolute(), 'job_root must be absolute.')
    root = Path(value['job_root'])
    require(root.is_dir() and not root.is_symlink(), 'job_root must be an existing host-owned real directory.')
    grant = value['model_grant']
    fields(grant, 'profile model_path model_manifest_id')
    require(grant['profile'] == PROFILE, 'Unsupported speaker model profile.', 'MODEL_UNAVAILABLE')
    require(isinstance(grant['model_path'], str) and Path(grant['model_path']).is_absolute(), 'Model path must be an absolute local directory.')
    require(Path(grant['model_path']).is_dir(), 'Local ECAPA model directory is missing; install or disable the optional component.', 'MODEL_UNAVAILABLE')
    require(isinstance(grant['model_manifest_id'], str) and grant['model_manifest_id'].startswith('sha256:'), 'Model manifest must identify local checkpoint bytes.')
    sha(grant['model_manifest_id'][7:])
    return value


def validate_run(value):
    fields(value, 'job_id attempt remaining_budget_ms session_id track_id segment_id segment_revision pcm overlap')
    for key in ('job_id', 'session_id', 'track_id', 'segment_id'):
        uuid(value[key])
    integer(value['attempt'], 1, 1000)
    integer(value['segment_revision'], 1, 2**53 - 1)
    integer(value['remaining_budget_ms'], 1, 120000)
    require(type(value['overlap']) is bool, 'overlap must be an explicit boolean.')
    pcm = value['pcm']
    fields(pcm, 'relative_path sha256 sample_rate channels format')
    require(pcm['relative_path'] == 'segment.pcm', 'Only the owned segment.pcm input is permitted.')
    sha(pcm['sha256'])
    require(type(pcm['sample_rate']) is int and pcm['sample_rate'] == 16000 and type(pcm['channels']) is int and pcm['channels'] == 1 and pcm['format'] == 's16le', 'PCM must be mono 16000 Hz signed 16-bit little endian.')
    return value


def load_pcm(configuration, params):
    root = Path(configuration['job_root'])
    # Open every component by directory descriptor: no symlink traversal, even
    # if a task directory is swapped while a hostile writer races this read.
    fds = []
    try:
        fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        fds.append(fd)
        for part in (params['job_id'], str(params['attempt'])):
            fd = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
            fds.append(fd)
        fd = os.open('segment.pcm', os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=fd)
        fds.append(fd)
        info = os.fstat(fd)
        require(stat.S_ISREG(info.st_mode) and 0 < info.st_size <= MAX_PCM_BYTES and info.st_size % 2 == 0, 'PCM is empty, not a regular file, odd-sized or longer than 30 seconds.', 'INVALID_AUDIO')
        data = bytearray()
        while len(data) < info.st_size:
            chunk = os.read(fd, min(65536, info.st_size - len(data)))
            require(bool(chunk), 'PCM changed while reading.', 'INVALID_AUDIO')
            data.extend(chunk)
        require(not os.read(fd, 1), 'PCM changed while reading.', 'INVALID_AUDIO')
        require(hashlib.sha256(data).hexdigest() == params['pcm']['sha256'], 'PCM hash does not match immutable input.', 'INVALID_AUDIO')
    except OSError as exc:
        raise WorkerError('INVALID_AUDIO', 'Cannot read the owned regular PCM input: ' + type(exc).__name__) from None
    finally:
        for fd in reversed(fds):
            os.close(fd)
    samples = array.array('h', data)
    if sys.byteorder != 'little':
        samples.byteswap()
    rms = math.sqrt(sum((v / 32768.0) ** 2 for v in samples) / len(samples))
    clipping = sum(abs(v) >= 32700 for v in samples) / len(samples)
    return samples, {'duration_seconds': len(samples) / 16000, 'rms': rms, 'clipping_ratio': clipping}


def result_base(configuration, params, quality):
    return {'schema_version': 1, **{k: params[k] for k in ('job_id', 'attempt', 'session_id', 'track_id', 'segment_id', 'segment_revision')},
            'model_manifest_id': configuration['model_grant']['model_manifest_id'], 'pcm_sha256': params['pcm']['sha256'],
            'status': 'unknown', 'reason': None, 'embedding': None, 'quality': quality}


def audio_rejection(params, quality):
    if params['overlap']:
        return 'overlap', 'host_reported_overlap'
    if quality['duration_seconds'] < 1.5:
        return 'unknown', 'too_short'
    if quality['rms'] < 0.005:
        return 'unknown', 'quiet_or_silent'
    if quality['clipping_ratio'] > 0.01:
        return 'unknown', 'clipped_audio'
    return None
