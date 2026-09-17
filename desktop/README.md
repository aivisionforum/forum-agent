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
translation, model downloader, Metal library, audio library and shell resources.
The local build is ad-hoc signed; it is not notarized or a finished distribution.
The Python analysis interpreter/MLX runtime is not bundled yet. Model weights are
prepared separately before offline use.

For layout development, `npm --prefix forum-shell/ui run dev` opens a loopback
Vite server. Browser mode is visibly marked as synthetic preview and cannot
start real capture. `npm --prefix forum-shell/ui run desktop:dev` launches the
Tauri development shell; it does not automatically start capture.

## Runtime boundary

The current production Dora controller attaches to an existing shared network
and refuses to start if a flow is already running. It no longer scans/stops
unrelated Translator flows or destroys a shared coordinator. Automatic private
coordinator/daemon supervision is still an F01 task. A developer can build or
inspect the UI without a Dora network, but a build alone does not make the live
capture path ready.

Existing complete `.OminiX` models can be selected read-only for inference.
New downloads belong under
`~/Library/Application Support/AI Vision Forum/models`; the installer must not
repair or clear another application's model directory. Explicit model path
overrides are inference inputs, not authorization to overwrite an external
cache. Full revision/checksum installation and concurrent-cache acceptance are
still pending in F01/F11.

The independent [meeting worker](../services/meeting-worker/README.md) currently
implements its process protocol only. The [core library](crates/forum-core/README.md)
implements the documented F02-a persistence subset only. Neither is connected to
the imported UI/runtime yet.

Run model probes separately from unit tests; they never open microphones. Their
small synthetic examples are not real-time latency or meeting-quality acceptance.
