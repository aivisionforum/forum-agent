#!/usr/bin/env python3
"""Emit NUL-separated owned Mach-O/FAT/MetalLib paths for inside-out signing.

Reads four-byte native file magic in one Python process, avoiding thousands of
file(1) subprocesses for text modules. The main executable is signed by the final
app-container operation, after all nested code. Symlinks are never followed.
"""
from __future__ import annotations
import argparse
import os
from pathlib import Path
import stat
import sys

MACH_MAGICS = {b'\xfe\xed\xfa\xce', b'\xce\xfa\xed\xfe', b'\xfe\xed\xfa\xcf', b'\xcf\xfa\xed\xfe'}
FAT_MAGICS = {b'\xca\xfe\xba\xbe', b'\xbe\xba\xfe\xca', b'\xca\xfe\xba\xbf', b'\xbf\xba\xfe\xca'}
METAL_MAGIC = b'MTLB'


def code_kind(magic):
    if magic in MACH_MAGICS:
        return 'Mach-O'
    if magic in FAT_MAGICS:
        return 'FAT'
    if magic == METAL_MAGIC:
        return 'MetalLib'
    return None


def native_paths(app):
    app = Path(app).resolve(strict=True)
    root = app / 'Contents'
    if not root.is_dir() or root.is_symlink():
        raise ValueError('Expected a real app Contents directory.')
    main = root / 'MacOS/forum-shell'
    result = []
    for directory, directories, files in os.walk(root, followlinks=False):
        directories[:] = [name for name in directories if not (Path(directory) / name).is_symlink()]
        for name in files:
            path = Path(directory) / name
            if path == main or path.is_symlink():
                continue
            descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
            try:
                if stat.S_ISREG(os.fstat(descriptor).st_mode) and code_kind(os.read(descriptor, 4)):
                    result.append(path)
            finally:
                os.close(descriptor)
    return sorted(result)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app', required=True, type=Path)
    args = parser.parse_args()
    for path in native_paths(args.app):
        sys.stdout.buffer.write(os.fsencode(path) + b'\0')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
