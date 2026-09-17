# Third-party source provenance

The Forum integration includes selected source files and runtime resources from
[Hen Local Translator](https://github.com/Hen-Local/Hen-Local-Translator), commit
`49042697f11bc86dfa8e6bd10ef0dc07cffbaf84`, under its Apache-2.0 repository license.
The imported license is preserved at `desktop/LICENSE`.

The original file paths, Git blob IDs and SHA-256 hashes before Forum adaptations
are recorded in `docs/engineering/hen-import-manifest.json`. The import reads Git
objects at the pinned commit, never the source checkout's uncommitted contents.
Forum-specific edits are maintained in this repository's history.

Imported modules include the Tauri/Svelte shell, Dora audio bridge, Qwen ASR and
translation nodes, model downloader and selected macOS packaging scripts.
Account credentials, subscription implementation, experimental voice lab,
generated build outputs and model weights are excluded. Existing upstream icons
are retained as development placeholders; Forum branding is a later UI task.

OminiX-MLX dependencies retain upstream commit
`6aac996db8b71fb7dae7a2409c46b4f2ade93092`. Cargo and npm lockfiles record transitive
dependency versions; their individual licenses continue to apply. This file is
an import record, not a completed redistribution audit of all dependencies.

The upstream tracked `desktop/moxin-dora-bridge/lib/libAudioCapture.dylib` is an
arm64 legacy runtime resource. F11 inspection found its minimum macOS version
is 15; it is excluded from the current Forum bundle, whose reliable capture
path does not use that library. The imported file remains for provenance review.
Models are obtained separately
and require their own manifest, revision and license records before release.

## Standalone meeting worker runtime

The F01 development bundle includes Astral python-build-standalone CPython
3.12.11 and hash-pinned Python wheels. Exact inputs and source URLs are recorded
in `services/meeting-worker/packaging/runtime-macos-arm64.lock.json`. The bundle
preserves CPython and static dependency license files under
`Contents/Resources/meeting-worker/third-party/python/`, wheel license/NOTICE
files in their original dist-info layouts, and an inventory at
`Contents/Resources/meeting-worker/THIRD_PARTY.md`. Build-only tools and model
weights are excluded. This inventory does not replace F11 distribution review.

## Standalone speaker worker runtime

The optional F09 ECAPA worker has a separate CPython 3.12.11/PyTorch environment;
it does not replace the meeting/ASR environment. Exact upstream archives, wheels,
versions and SHA256 values are recorded in
`services/speaker-worker/packaging/runtime-macos-arm64.lock.json`. The package
preserves interpreter/dependency licenses and wheel dist-info license layouts,
with its inventory at `Contents/Resources/speaker-worker/THIRD_PARTY.md`.
The ECAPA checkpoint is supplied separately and is not part of the app/DMG.
The offline preparation tool verifies the supported checkpoint's content hash;
that check does not itself grant rights to redistribute model weights.
