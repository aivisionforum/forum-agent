"""Versioned prompt bytes: host hashes exactly load_prompt(kind)."""
from pathlib import Path

from .job_io import KINDS, require, digest


def load_prompt(kind):
    require(kind in KINDS, 'Unsupported analysis kind.')
    return (Path(__file__).parent / 'prompts' / 'v1' / f'{kind}.txt').read_text(encoding='utf-8')


def manifest():
    return {kind: {'prompt_version': f'{kind}-v1', 'prompt_sha256': digest(load_prompt(kind).encode())} for kind in KINDS}
