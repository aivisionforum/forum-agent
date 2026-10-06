# Optional local speaker worker (F09)

`forum-speaker-worker` calculates real ECAPA speaker embeddings in an isolated
Python child. It has no meeting database, persistent centroids, speaker names,
server routes, browser access or dependency on the legacy Forum application.
Core assigns anonymous labels **within one session** and versions assignments.
A microphone/system track is a source, never a speaker identity.

The runtime is optional. Disabling it leaves ASR/translation usable. Initialization
and the transport use only Python's standard library; missing optional model or
runtime dependencies produce explicit errors, never an automatic model download.
The desktop host must expose the disabled/error state and retain source text.

## Model and isolation

The only profile is `ecapa-voxceleb-v1`, corresponding to the embedding network in
`speechbrain/spkrec-ecapa-voxceleb`:

- 16 kHz mono audio; 80-mel Fbank, sentence mean normalization;
- ECAPA channels `[1024,1024,1024,1024,3072]`, kernels `[5,3,3,3,1]`, dilations
  `[1,2,3,4,1]`, attention channels 128, output dimension 192;
- CPU inference, at most two Torch compute threads; L2 normalized output;
- only `embedding_model.ckpt` is consumed; the manifest is
  `sha256:` plus the SHA-256 of its exact resolved bytes;
- fixed Python architecture, `torch.load(weights_only=True)` and strict tensor
  state dictionary loading. No `from_hparams`, HyperPyYAML object loading,
  checkpoint-defined imports, classifier, names or hub fetches.

The original `forum_agent.diarize.Diarizer` policy is not reused: it assigned short
utterances to the last speaker and forced a match when clusters were full. Here,
short/silent/clipped audio stays unknown. Known overlap is passed through as
`overlap`; **ECAPA does not detect overlapping speakers by itself**. A normal
embedding is not a calibrated confidence probability and cannot establish a
person's identity.

`packaging/local-runtime-observation.json` records the installed dependency set
used for the initial offline probe. It is an observation, **not** a hash-locked
release runtime or proof that the dependency resolver accepts all versions.
A portable distribution requires the host packaging step's resolved wheel lock.
The host runs the installed module with its own portable interpreter, for example:

```sh
/path/to/portable/python3.12 -I -B -m forum_speaker_worker
```

For repository development, install the package into a separate local environment
or use `PYTHONPATH=services/speaker-worker/src python -m forum_speaker_worker`.
The normal desktop launcher should use `-I` and the installed package.

## Protocol version 1

One JSON-RPC 2.0 object per LF-terminated UTF-8 line. Maximum frame size is 64 KiB.
An incomplete frame has a 10-second absolute deadline (`--frame-timeout-seconds`
can shorten it for host tests). Duplicate JSON keys, NaN/Infinity, invalid identity
and unknown fields are rejected. Stdout is protocol only; diagnostic output is
stderr. A worker owns at most one immutable `job_id`/`attempt`; retries create a
new worker. Caller-controlled model paths are not accepted in `jobs.run`.

Initialize does not load a model:

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocol_version":1,"instance_id":"00000000-0000-0000-0000-000000000001","job_root":"/private/host-owned/jobs","model_grant":{"profile":"ecapa-voxceleb-v1","model_path":"/private/installed/ecapa-model","model_manifest_id":"sha256:<64 lowercase hex>"}}}
```

Run against `job_root/<job_id>/<attempt>/segment.pcm`:

```json
{"jsonrpc":"2.0","id":2,"method":"jobs.run","params":{"job_id":"00000000-0000-0000-0000-000000000002","attempt":1,"remaining_budget_ms":30000,"session_id":"00000000-0000-0000-0000-000000000003","track_id":"00000000-0000-0000-0000-000000000004","segment_id":"00000000-0000-0000-0000-000000000005","segment_revision":1,"pcm":{"relative_path":"segment.pcm","sha256":"<64 lowercase hex>","sample_rate":16000,"channels":1,"format":"s16le"},"overlap":false}}
```

The PCM is raw signed 16-bit little endian, **not a WAV container**. It must be
nonempty, even-sized and at most 30 seconds (960,000 bytes). The worker opens each
job/attempt component with `O_NOFOLLOW` and reads only a bounded regular file by
descriptor, including nonblocking rejection of FIFOs. It checks exact content
hash before inference. The host binds the file to the stable segment revision and
must revalidate that revision before committing an assignment.

A successful run returns an inline `result` with:

```json
{"schema_version":1,"job_id":"<same job UUID>","attempt":1,"session_id":"<same session UUID>","track_id":"<same track UUID>","segment_id":"<same segment UUID>","segment_revision":1,"model_manifest_id":"sha256:<granted checkpoint hash>","pcm_sha256":"<input hash>","status":"embedding","reason":null,"embedding":[0.01,0.02],"quality":{"duration_seconds":3.0,"rms":0.15,"clipping_ratio":0.0}}
```

The two-element vector above is abbreviated for readability; an actual embedding
contains exactly **192 finite floats** and has L2 norm 1. Other results have
`embedding:null` and one of these explicit states:

| State | Reason | Current development gate |
| --- | --- | --- |
| `unknown` | `too_short` | Less than 1.5 seconds |
| `unknown` | `quiet_or_silent` | PCM RMS below 0.005 |
| `unknown` | `clipped_audio` | More than 1% of samples at absolute amplitude ≥32700 |
| `overlap` | `host_reported_overlap` | Host explicitly marked overlap |

These are conservative signal gates, not a voice activity detector or calibrated
acoustic quality score. Non-speech can still yield an embedding; the host should
only submit stable spoken segments. Clustering thresholds and ambiguous matches
belong to the session-scoped core policy and need real speaker evaluation.

The same connection accepts `health.ping`, `jobs.cancel` with `{job_id,attempt}`,
and `shutdown` with `{}`. Ping is responsive during native model loading and
inference. Cancel acknowledges `cancel_requested`, then the original run replies
with `CANCELLED` **after the owned compute child is reaped**. A total job budget
of 1–120,000 ms covers startup, validation, model loading and inference.

The compute child inherits the supervisor's process group and creates no new
session. On cancel/deadline/EOF the control plane terminates its exact child,
waits 500 ms, escalates to kill, then reaps it. If native cleanup does not return,
the non-daemon controller remains owned: the Rust host must enforce its independent
process-group deadline. Neither a shutdown acknowledgment nor a closed pipe is
proof that all owned computation exited.

Structured errors include `MODEL_UNAVAILABLE`, `MODEL_MISMATCH`,
`DEPENDENCY_UNAVAILABLE`, `MODEL_INFERENCE_FAILED`, `INVALID_AUDIO`,
`INVALID_EMBEDDING`, `CANCELLED`, `DEADLINE_EXCEEDED`, and `RESOURCE_BUSY`.
The host must preserve missing/error/unknown states instead of assigning the last
speaker or silently substituting the capture track name.

## Verification

Standard-library tests (no models or network):

```sh
PYTHONPATH=services/speaker-worker/src python3.12 -m unittest discover -s services/speaker-worker/tests -v
```

An explicit real-model probe, using a model that is already installed:

```sh
python3.12 services/speaker-worker/tests/run_local_embedding_probe.py \
  --python /absolute/path/to/python3.12 \
  --model /absolute/path/to/ecapa-model \
  --output /absolute/path/to/probe-report.json
```

Add `--installed` to verify the packaged module rather than the repository source.
This creates a three-second waveform, computes an actual embedding, checks its
192 dimensions/L2 norm, pings during inference, shuts down, and verifies the model
cache size/mtime did not change. It does not use a microphone or download a model.
The generated waveform proves execution and transport; it does not establish
speaker separation accuracy, overlap detection, or formal C3 acceptance.

## Build an independent portable runtime

```sh
python3.12 desktop/scripts/package_speaker_worker.py \
  --output /absolute/new/speaker-worker \
  --cache /absolute/wheel-and-python-cache
```

After populating the pinned artifact cache, add `--offline` to prohibit artifact
fetches. The output must not exist; existing folders/environments are never
removed, overwritten or hard-linked. Each build extracts its own standalone
CPython 3.12.11 and installs the hash-locked dependency closure. It includes no
model weights, network model installer, development checkout or user virtualenv.
The existing meeting/ASR runtime remains independent.

The packager runs an installed-module/native-loader/lifecycle check before
publishing the new folder. The check can be repeated on the result:

```sh
/absolute/new/speaker-worker/python/bin/python3.12 -I -B \
  /absolute/new/speaker-worker/checks/check_bundle.py \
  --root /absolute/new/speaker-worker
```

A model probe is a separate, explicit step using the already installed ECAPA model
and `run_local_embedding_probe.py --installed`. The application build must copy
this runtime as its own `speaker-worker` resource and sign all native contents.
Distribution to a clean Mac, signing/notarization and real acoustic quality remain
separate product acceptance gates. License details and exact native-loader
relocations are documented in `packaging/THIRD_PARTY.md` and the runtime manifest.


Fresh macOS bundle checks allow 300 seconds per isolated Python subprocess for
first-load native-library scanning. Override with
`FORUM_SPEAKER_CHECK_TIMEOUT_SECONDS=600` (a positive finite number of seconds).
Both the packager and app runtime checker forward this one variable through
their sanitized environments; the app wrapper allows all three child checks
plus inspection time. This is a packaging diagnostic limit, not an inference
latency setting. A colleague reported successful second-Mac build/startup smoke
testing, including Node 22; acoustic quality and formal release gates remain
separate.
