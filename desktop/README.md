# AI Vision Forum desktop — development foundation

This workspace imports the selected Hen translation runtime at the revision in
[the source manifest](../docs/engineering/hen-import-manifest.json). Forum changes
live here; the original Translator checkout is not modified.

Current scope is a local translation shell plus independent, tested building
blocks. The shell does not yet read the meeting database or generate insights
and minutes. See [implementation progress](../docs/engineering/PROGRESS_ZH.md)
before interpreting a successful build as a finished product.

## Build and check

Development baseline: macOS Apple Silicon, Rust 1.92.0, Node 20.20.0. Run these
commands from this `desktop/` directory. Cargo's target path must be absolute and
must not contain spaces because of the imported MLX native build.

```bash
npm --prefix forum-shell/ui ci
npm --prefix forum-shell/ui run check
npm --prefix forum-shell/ui run test:p0
npm --prefix forum-shell/ui run build
CARGO_TARGET_DIR=/tmp/aivf-cargo-target cargo check --locked -p forum-shell
```

The project selects native arm64 Dora CLI 0.4.1, normally from
`$HOME/.cargo/bin/dora`. A deliberate `FORUM_AGENT_DORA_BIN` override must also
resolve to that native version. A Conda/Python wrapper on PATH is not accepted.

Build a development `.app`, without launching it:

```bash
FORUM_AGENT_CARGO_TARGET_DIR=/tmp/aivf-cargo-target \
  bash scripts/build_macos_app.sh --profile dev
```

Output: `dist/AI Vision Forum.app`. The bundle contains native Dora, ASR,
translation, model downloader, Metal library, audio library and shell resources,
plus standalone CPython 3.12.11, MLX 0.32.2 / mlx-lm 0.31.3 and the meeting worker.
The Python runtime is built from hash-pinned upstream archives and wheels, not
copied from a development virtual environment. First-time packaging downloads
these software dependencies; it does not download model weights.

The build checks the final worker resource directory with development environment
paths removed, including its protocol handshake, native library dependencies and
a small Metal matrix calculation. This is local isolation evidence: testing on
a second clean Mac is still pending. Users should not need to install Python,
Conda or developer tools to use the packaged worker. The local build is ad-hoc
signed; it is not notarized or a finished distribution. Model weights must be
prepared separately before offline use.

For layout development, `npm --prefix forum-shell/ui run dev` opens a loopback
Vite server. Browser mode is visibly marked as synthetic preview and cannot
start real capture. `npm --prefix forum-shell/ui run desktop:dev` launches the
Tauri development shell; it does not automatically start capture.

On macOS, the development launcher uses the repository's `.venv/bin/python`
for the ASR, translation and meeting workers, with adapters loaded from the
checkout. Prepare that environment with the pinned worker dependencies,
including `mlx-qwen3-asr==0.4.4`; `FORUM_AGENT_DEV_PYTHON` can select another
absolute Python path. Explicit per-worker Python/script overrides are retained.
Startup checks the ASR/translation imports and never downloads model weights.
The UI reports a missing runtime separately from missing model files.

## Runtime boundary

The desktop Dora controller creates its own coordinator and daemon on private
loopback ports. It verifies the flow UUID and daemon identity when registering
dynamic nodes; it does not attach to an existing Translator network or discover
processes to stop by name. Executable model nodes register as dynamic nodes and
are spawned as directly owned child processes, each in its own process group.

Stop completes only after the owned processes are reaped and bridge workers
have joined. A timed-out cleanup keeps its handles for retry and prevents a new
session from replacing live workers. CLI acknowledgement and fallback shutdown
are recorded separately. Device capture, OS permission prompts and long-running
sessions still require manual acceptance; a build or synthetic probe does not
prove those checks passed.

Existing complete `.OminiX` models can be selected read-only for inference.
New downloads belong under
`~/Library/Application Support/AI Vision Forum/models`; the installer must not
repair or clear another application's model directory. Explicit model path
overrides are inference inputs, not authorization to overwrite an external
cache. Full revision/checksum installation and concurrent-cache acceptance are
still pending in F01/F11.

The independent [meeting worker](../services/meeting-worker/README.md) currently
implements handshake and health checks, plus an explicitly enabled local MLX
packaging probe. Production `jobs.run` and `jobs.cancel` report
`CAPABILITY_UNAVAILABLE`; insight, minutes, report and queue processing are not
implemented. The [core library](crates/forum-core/README.md) implements the
documented F02-a persistence subset only. Neither the worker's analysis workflow
nor the meeting database is connected to the imported UI/runtime yet.

The current desktop requires an explicit source language. Whisper auto is a
candidate for a future ASR adapter, not a capability of this bundle; its Python
dependencies and weights are not bundled. The pinned Qwen binding must not label
configured language as detected language. See the
[F01 model findings](../docs/engineering/F01_MODEL_FINDINGS_ZH.md) for mixed-language
results, concurrency measurements and the summary hallucination that motivates
evidence checks and human review.

Run model probes separately from unit tests; they never open microphones. Their
small synthetic examples are not real-time latency or meeting-quality acceptance.

Test-kit copies must preserve macOS extended attributes: use `ditto --rsrc
--extattr` for the `.app`, not a generic recursive file copier. Archive an assembled
kit with `scripts/archive_f01_test_kit.sh /absolute/path/to/kit`; it preserves resource
metadata, verifies the signature after extraction, and writes a SHA-256 file.
