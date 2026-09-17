"""Bounded, immutable host snapshots and durable attempt-local outputs."""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import stat
from uuid import UUID, uuid4

MAX_FILE_BYTES = 32 * 1024 * 1024
KINDS = ('insight', 'minutes', 'event_report', 'suggested_questions',
         'redaction_review', 'closing_brief')


class JobError(Exception):
    def __init__(self, code: str, message: str):
        super().__init__(message)
        self.code = code


def require(condition: bool, message: str, code: str = 'INVALID_PARAMS') -> None:
    if not condition:
        raise JobError(code, message)


def canonical(value: object) -> bytes:
    return json.dumps(value, ensure_ascii=False, sort_keys=True,
                      separators=(',', ':'), allow_nan=False).encode('utf-8')


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def strict_json(data: bytes) -> object:
    def pairs(items):
        result = {}
        for key, value in items:
            require(key not in result, 'Duplicate JSON field.', 'INVALID_SNAPSHOT')
            result[key] = value
        return result
    def constant(_):
        raise JobError('INVALID_SNAPSHOT', 'Non-finite JSON value.')
    try:
        return json.loads(data, object_pairs_hook=pairs, parse_constant=constant)
    except (ValueError, UnicodeError, RecursionError):
        raise JobError('INVALID_SNAPSHOT', 'Invalid bounded JSON file.') from None


def uuid(value: object) -> str:
    try:
        require(isinstance(value, str), 'Expected UUID.')
        parsed = UUID(value)
        require(parsed.int != 0 and str(parsed) == value, 'Expected canonical non-nil UUID.')
        return value
    except (ValueError, AttributeError):
        raise JobError('INVALID_PARAMS', 'Expected canonical non-nil UUID.') from None


def sha(value: object) -> str:
    require(isinstance(value, str) and len(value) == 64
            and all(c in '0123456789abcdef' for c in value), 'Expected lowercase SHA256.')
    return value


def integer(value, minimum, maximum):
    require(type(value) is int and minimum <= value <= maximum, 'Integer outside supported bounds.')
    return value


class AttemptFiles:
    """Every access traverses dirfds with O_NOFOLLOW; no path check/use gap.

    Host creates job_root/job_uuid/attempt ahead of admission. Output names are
    flat and create-only; confirmed checkpoints may read another attempt of
    the same job, never another job or arbitrary filesystem path.
    """
    def __init__(self, root: str, job_id: str, attempt: int):
        # The host root may use macOS's /tmp or /var system aliases. Resolve
        # that trusted root once, but never resolve job/attempt/input symlinks.
        self.root = str(Path(root).resolve(strict=True))
        self.job_id, self.attempt = uuid(job_id), integer(attempt, 1, 1000)

    def _directory(self, attempt=None):
        root = Path(self.root)
        require(root.is_absolute(), 'Host root must be absolute.', 'INVALID_SNAPSHOT')
        fd = os.open(root.anchor, os.O_RDONLY | os.O_DIRECTORY)
        try:
            for part in (*root.parts[1:], self.job_id, str(attempt or self.attempt)):
                require(part not in ('.', '..'), 'Invalid directory component.', 'INVALID_SNAPSHOT')
                next_fd = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
                os.close(fd)
                fd = next_fd
            metadata = os.fstat(fd)
            require(metadata.st_uid == os.getuid() and not metadata.st_mode & 0o077,
                    'Attempt directory must be owned and mode 0700.', 'INVALID_SNAPSHOT')
            return fd
        except BaseException:
            os.close(fd)
            raise

    def read(self, name: str, expected_hash: str, *, attempt=None) -> bytes:
        require(isinstance(name, str) and name not in ('', '.', '..')
                and Path(name).name == name and '\\' not in name,
                'Snapshot/checkpoint path must be an attempt-local filename.', 'INVALID_SNAPSHOT')
        sha(expected_hash)
        directory = self._directory(attempt)
        try:
            fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=directory)
            with os.fdopen(fd, 'rb') as stream:
                before = os.fstat(stream.fileno())
                require(stat.S_ISREG(before.st_mode) and before.st_size <= MAX_FILE_BYTES,
                        'Expected bounded regular input file.', 'INVALID_SNAPSHOT')
                data = stream.read(MAX_FILE_BYTES + 1)
                after = os.fstat(stream.fileno())
                require(len(data) == before.st_size <= MAX_FILE_BYTES
                        and before.st_mtime_ns == after.st_mtime_ns and before.st_size == after.st_size
                        and digest(data) == expected_hash, 'Input bytes changed or hash mismatched.', 'INVALID_SNAPSHOT')
                return data
        finally:
            os.close(directory)

    def write(self, name: str, value: dict) -> str:
        require(Path(name).name == name and name not in ('', '.', '..'), 'Invalid output filename.')
        data = canonical(value)
        require(len(data) <= MAX_FILE_BYTES, 'Result exceeds file limit.', 'INVALID_MODEL_OUTPUT')
        directory = self._directory()
        temporary = f'.{uuid4()}.tmp'
        try:
            fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=directory)
            with os.fdopen(fd, 'wb') as stream:
                stream.write(data)
                stream.flush()
                os.fsync(stream.fileno())
            # Hard-link is create-only, unlike rename which can overwrite a
            # previous result from the same immutable attempt.
            os.link(temporary, name, src_dir_fd=directory, dst_dir_fd=directory, follow_symlinks=False)
            os.unlink(temporary, dir_fd=directory)
            os.fsync(directory)
            return digest(data)
        finally:
            try:
                os.unlink(temporary, dir_fd=directory)
            except FileNotFoundError:
                pass
            os.close(directory)


def validate_grants(grants: object, allow_fake: bool) -> list[dict]:
    require(isinstance(grants, list) and len(grants) <= 3, 'Invalid model grants.')
    profiles = set()
    for grant in grants:
        require(isinstance(grant, dict) and set(grant) == {
            'profile', 'model_path', 'model_manifest_id', 'context_limit', 'max_output_tokens'}, 'Invalid grant fields.')
        profile = grant['profile']
        require(profile in ('meeting-8b-v1', 'meeting-32b-v1') or
                (allow_fake and profile == 'test-fake-v1'), 'Unsupported model profile.')
        require(profile not in profiles, 'Duplicate model grant.')
        profiles.add(profile)
        require(isinstance(grant['model_path'], str) and Path(grant['model_path']).is_absolute(), 'Local model path required.')
        require(isinstance(grant['model_manifest_id'], str) and grant['model_manifest_id'].startswith('sha256:'), 'Actual model digest required.')
        sha(grant['model_manifest_id'][7:])
        integer(grant['context_limit'], 512, 131072)
        integer(grant['max_output_tokens'], 64, 16384)
    return grants
