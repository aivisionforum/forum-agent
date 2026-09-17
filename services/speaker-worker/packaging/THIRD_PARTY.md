# Speaker runtime distribution inventory

This optional runtime is separate from the meeting/ASR runtime. It contains no
speaker model weights. `runtime-macos-arm64.lock.json` pins official release URLs,
SHA-256 values, sizes and versions for standalone CPython and every Python wheel.
The build installs with `--no-index --no-deps --require-hashes`, then runs
`pip check` against the complete installed dependency closure.

The portable artifact preserves upstream wheel license and NOTICE files in
`python/lib/python3.12/site-packages/`. Its `THIRD_PARTY.md` inventories each runtime
wheel and links the corresponding PyPI release metadata. CPython and its linked
static-library license texts are copied from the separately hash-pinned upstream
full Python distribution into `third-party/python/`; build objects are excluded.
Build-only packages stay outside the published runtime. This inventory does not
relicense any upstream component or claim legal clearance for separate models.

The implementation reuses the existing repository's hash-pinned download,
standalone archive extraction, wheel install and Mach-O audit primitives. Their
exact source digests are recorded in the runtime manifest. The speaker packager
never invokes the meeting packager's `package()` function, modifies its globals,
changes an existing Python environment, or shares mutable native files by hardlink.

Known native-loader fixes are limited to the pinned wheels and included in the
runtime manifest with before/after hashes:

- SciPy 1.18.1: remove the three already documented unused Homebrew GCC build
  rpaths, while preserving its relative bundled BLAS/Fortran dependencies.
- Torchaudio 2.11.0: replace the exact EC2/Conda build path in `_torchaudio.abi3.so`
  and `libtorchaudio.abi3.so` with `@loader_path/../../torch/lib`, after confirming
  every `@rpath` dependency exists there. Re-sign those two modified files without
  a timestamp and update their wheel RECORD rows.

The bundled checker scans all native `.so`/`.dylib` files and the Python executable
for an arm64 slice, macOS ≤14 minimum, and system/relative loader paths. It also
runs isolated Python path checks, import/version checks and the worker lifecycle
protocol without model loading. This is local distribution evidence, not clean-Mac
installation, microphone permission, acoustic diarization or notarization approval.

The model profile expects the separately installed embedding checkpoint from
`speechbrain/spkrec-ecapa-voxceleb`. The worker does not download it. That model's
upstream model card/license and intended use must accompany any future model
installer/distributor. The worker ignores its YAML, classifier and name labels.
