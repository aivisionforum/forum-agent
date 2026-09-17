#!/usr/bin/env python3
"""Stage/build an isolated Qwen auto-language experiment without editing production.

Copies only the pinned ASR crate into an ignored artifact directory. Other MLX
dependencies point to the existing pinned checkout. Network and downloads are
disabled during the build; no model is loaded by this script.
"""
import argparse
import difflib
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[1]
REVISION = "6aac996db8b71fb7dae7a2409c46b4f2ade93092"


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source',required=True,help='Existing pinned OminiX checkout directory')
    parser.add_argument('--output',default=str(ROOT/'artifacts/local/f01-qwen-auto-experiment'))
    parser.add_argument('--mlx-prebuilt',required=True)
    parser.add_argument('--target',default='/tmp/aivf-auto-probe-target')
    parser.add_argument('--seed-language-marker',action='store_true',help='Seed only the protocol word language, never a language value')
    args=parser.parse_args()
    source=Path(args.source).resolve(strict=True)
    observed=subprocess.check_output(['git','rev-parse','HEAD'],cwd=source,text=True).strip()
    if observed != REVISION:
        raise ValueError(f'Expected pinned {REVISION}, got {observed}')
    output=Path(args.output).resolve()
    output.mkdir(parents=True,exist_ok=True)
    crate=output/'qwen3-asr-mlx'
    if crate.exists():
        raise ValueError('Use a new output directory; an existing experiment is preserved')
    shutil.copytree(source/'qwen3-asr-mlx',crate)
    cargo=crate/'Cargo.toml'
    manifest=cargo.read_text()
    for relative in ['../mlx-rs/mlx-sys','../mlx-rs-core','../mlx-rs']:
        manifest=manifest.replace(f'path = "{relative}"',f'path = {json.dumps(str((source/"qwen3-asr-mlx"/relative).resolve()))}')
    cargo.write_text(manifest)
    model=crate/'src/model.rs'
    original=model.read_text()
    # Match the exact pinned prefix. Empty-string behavior here follows the
    # official optional-language prompt, unlike the production literal prefix.
    needle='''        let encoding = tokenizer.encode(prompt.as_str(), false)'''
    assert original.count(needle)==1
    changed=original.replace(needle,'''        let prompt = if language.is_empty() {
            prompt.strip_suffix("language <asr_text>").expect("pinned prompt suffix").to_string()
        } else {
            prompt
        };
        let encoding = tokenizer.encode(prompt.as_str(), false)''')
    assert changed.count('.decode(&token_ids, true)')==1
    changed=changed.replace('.decode(&token_ids, true)','.decode(&token_ids, false)')
    if args.seed_language_marker:
        changed=changed.replace('expect("pinned prompt suffix").to_string()', 'expect("pinned prompt suffix").to_string() + "language "')
    model.write_text(changed)
    (output/'auto-language.patch').write_text(''.join(difflib.unified_diff(original.splitlines(True),changed.splitlines(True),fromfile='pinned/src/model.rs',tofile='experiment/src/model.rs')))
    (output/'src').mkdir()
    shutil.copyfile(ROOT/'desktop/node-hub/dora-qwen3-asr/examples/forum_asr_auto_probe.rs',output/'src/main.rs')
    (output/'Cargo.toml').write_text('''[package]
name = "forum-asr-auto-experiment"
version = "0.0.0"
edition = "2021"
[workspace]
[dependencies]
anyhow = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
qwen3-asr-mlx = { path = "qwen3-asr-mlx" }
''')
    provenance={'pinned_revision':REVISION,'source_path':str(source),'original_model_sha256':hashlib.sha256(original.encode()).hexdigest(),'patched_model_sha256':hashlib.sha256(changed.encode()).hexdigest(),'patch_scope':['omit forced assistant suffix for empty hint','decode protocol separator without dropping special tokens'],'seed_language_marker':args.seed_language_marker,'production_files_changed':False,'model_weights_changed':False}
    (output/'provenance.json').write_text(json.dumps(provenance,indent=2)+'\n')
    env={**os.environ,'FORUM_EXPERIMENTAL_AUTO_PATCH':'1','CARGO_TARGET_DIR':str(Path(args.target).resolve()),'MLX_PREBUILT_PATH':str(Path(args.mlx_prebuilt).resolve(strict=True))}
    with (output/'build.log').open('w') as log:
        subprocess.run(['cargo','+stable','build','--offline','--manifest-path',str(output/'Cargo.toml')],cwd=ROOT,env=env,stdout=log,stderr=subprocess.STDOUT,check=True)
    metal=Path(args.mlx_prebuilt)/'lib/mlx.metallib'
    if not metal.exists():
        candidates=list(Path(args.mlx_prebuilt).rglob('mlx.metallib'))
        if len(candidates)!=1:
            raise RuntimeError('Could not resolve one prebuilt MLX Metal library')
        metal=candidates[0]
    binary=Path(args.target).resolve()/'debug/forum-asr-auto-experiment'
    shutil.copyfile(metal,binary.parent/'mlx.metallib')
    print(binary)


if __name__=='__main__':
    main()
