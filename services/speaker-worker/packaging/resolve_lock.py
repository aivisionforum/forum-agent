#!/usr/bin/env python3
"""Explicit maintainer action: pin official PyPI wheels for the observed versions.

Requires packaging in the bootstrap environment. Never runs during app startup or
normal packaging. Reviews/tests and a real installed ECAPA probe must accompany a
lock refresh. It does not install packages or download model weights.
"""
import concurrent.futures
import json
from pathlib import Path
import urllib.request

from packaging.tags import compatible_tags, cpython_tags, mac_platforms
from packaging.utils import parse_wheel_filename

ROOT = Path(__file__).resolve().parents[3]
DIRECTORY = Path(__file__).resolve().parent


def main():
    existing = json.loads((ROOT / 'services/meeting-worker/packaging/runtime-macos-arm64.lock.json').read_text())
    observation = json.loads((DIRECTORY / 'local-runtime-observation.json').read_text())
    platforms = list(mac_platforms(version=(14, 0), arch='arm64'))
    tags = list(cpython_tags(python_version=(3, 12), platforms=platforms))
    tags += list(compatible_tags(python_version=(3, 12), interpreter='cp312', platforms=platforms))
    ranking = {tag: index for index, tag in reversed(list(enumerate(tags)))}
    def resolve(dependency):
        url = f"https://pypi.org/pypi/{dependency['name']}/{dependency['version']}/json"
        with urllib.request.urlopen(url, timeout=30) as response:
            data = json.load(response)
        candidates = []
        for artifact in data['urls']:
            if artifact['packagetype'] != 'bdist_wheel' or artifact.get('yanked'):
                continue
            _, _, _, wheel_tags = parse_wheel_filename(artifact['filename'])
            ranks = [ranking[tag] for tag in wheel_tags if tag in ranking]
            if ranks:
                candidates.append((min(ranks), artifact['filename'], artifact))
        if not candidates:
            raise RuntimeError('No compatible official wheel: ' + dependency['name'])
        artifact = sorted(candidates, key=lambda item: item[:2])[0][2]
        return {'name': data['info']['name'], 'version': dependency['version'], 'role': 'runtime',
            'filename': artifact['filename'], 'url': artifact['url'], 'sha256': artifact['digests']['sha256'],
            'size': artifact['size'], 'metadata_url': url}
    with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
        wheels = list(pool.map(resolve, observation['dependencies']))
    wheels += [wheel for wheel in existing['wheels'] if wheel['role'] == 'build']
    result = {key: existing[key] for key in ('schema_version', 'target', 'python', 'source_date_epoch', 'minimum_macos', 'python_license_archive')}
    result.update(component='speaker-worker', model_weights_included=False,
        wheels=sorted(wheels, key=lambda wheel: (wheel['role'], wheel['name'].lower())))
    (DIRECTORY / 'runtime-macos-arm64.lock.json').write_text(json.dumps(result, indent=2) + '\n')
    print(f'Pinned {len(wheels)} official wheels; no wheels or models installed.')


if __name__ == '__main__':
    main()
