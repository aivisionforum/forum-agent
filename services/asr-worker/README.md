# Local automatic ASR adapter

F03 adds a local Whisper adapter for `source_language=auto`. The existing Rust Qwen3 ASR remains the fixed-language backend. Detected language is separate from the configured hint; Whisper reports the dominant language of the segment, not word-level language identification. Mixed Chinese/English text is preserved for translation routing.

The desktop owns the Rust ASR node. That node owns a separate Python process running this adapter. It does not share a model interpreter with the meeting worker. `FORUM_ASR_PYTHON`, `FORUM_ASR_SCRIPT`, and `FORUM_WHISPER_MODEL_PATH` must select existing absolute paths. The model directory must contain `config.json` and `weights.safetensors`; a Hugging Face repository name or implicit download is rejected.

The packaged app includes the pinned standalone CPython/MLX runtime and adapter, but no weights. Development can explicitly select a built runtime:

```sh
export FORUM_ASR_PYTHON=/absolute/meeting-worker/python/bin/python3.12
export FORUM_ASR_SCRIPT=/absolute/meeting-worker/asr/asr_worker.py
export FORUM_WHISPER_MODEL_PATH=/absolute/existing/whisper-large-v3-turbo
```

Protocol version 1 is bounded newline-delimited JSON over owned stdio. Startup loads and evaluates the model before emitting `ready`. Inputs contain an ID, mono 16 kHz finite PCM in [-1, 1], and `language=auto` (up to 30 seconds). Results echo the ID and contain `text`, `detected_language`, and `status=success|empty|failed`. Shutdown is `{"type":"shutdown"}`. Stdout is reserved for protocol frames; library output is redirected to stderr. Frame assembly and Rust process I/O have deadlines. Native inference cancellation ultimately uses the owned ASR process group.

Digital silence returns an explicit empty result. Failure never becomes an empty transcript. The Rust producer durably writes final results before acknowledging core ingestion; pending work remains in the outbox when core is unavailable. The recording journal provides recovery for audio whose final result was never produced.

Build with `desktop/scripts/package_meeting_worker.py --with-asr`. The superset lock retains official wheels and licenses. One pinned SciPy native extension contains three unused Homebrew build RPATHs: the packager removes exactly those three, updates its RECORD hash, and signs the relocated extension. The bundle checker still rejects external non-system dependencies. This is local portability evidence; a clean second Mac remains a separate acceptance test.

Validation:

```sh
.venv/bin/python -m unittest discover -s services/asr-worker/tests
```

Real local probes use the existing synthetic corpus under ignored `artifacts/local/f01-model-feasibility/`. No meeting microphone is opened by those probes. A few synthetic cases do not establish live-meeting accuracy, latency percentiles, or long-session stability.
