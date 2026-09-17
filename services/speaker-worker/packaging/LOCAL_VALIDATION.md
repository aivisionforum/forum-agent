# Local F09 / F11 speaker validation — 2026-09-16

Engineering checks passed on the development Mac. These results are not formal
C3 speaker accuracy, clean-machine installation, or notarized distribution
acceptance. No microphone/system capture was opened and no model was downloaded.

- 22 standard-library tests passed, covering identity/attempt fences, immutable
  PCM hashes, source/attempt symlink and FIFO rejection, size and audio gates,
  ping/cancel responsiveness, owned child cleanup, startup deadline, malformed
  frames, pinned artifact validation, offline absence, and refusing output reuse.
- Actual ECAPA checkpoint bytes from the existing local cache were loaded with
  the fixed architecture and `weights_only=True`; the original Forum server,
  session module, YAML constructor and persistent legacy clustering were unused.
- Fresh standalone CPython 3.12.11 runtime built into
  `/tmp/forum-speaker-runtime-2`, using the hash-pinned artifact cache in offline
  mode. `pip check` passed. No development virtualenv or pre-existing meeting/ASR
  runtime was copied, modified or hard-linked into this runtime.
- The independent checker validated 151 native files for arm64/macOS-14 compatible
  headers and portable dependency paths, verified isolated `sys.path` and
  interpreter placement, checked exact installed dependency versions, imported
  Torch/Torchaudio/SpeechBrain without weights, and exercised the installed worker
  lifecycle protocol.
- The installed module, invoked by its own interpreter under `-I` with no source
  checkout path, processed a generated three-second tone waveform in **3.719 s**.
  Output: 192-dimensional L2-normalized embedding. A concurrent ping returned in
  **0.292 ms**. The compute child and controller exited, and the existing model
  file's size/mtime were unchanged. These timings are observations on this Mac,
  not calibrated real-time concurrency limits.
- Existing checkpoint fingerprint:
  `sha256:0575cb64845e6b9a10db9bcb74d5ac32b326b8dc90352671d345e2ee3d0126a2`.

The first package attempt correctly rejected Torchaudio's upstream EC2/Conda build
RPATH. The speaker packager now changes only those two pinned native files to use
the bundled Torch library path, preserves upstream notices, updates RECORD hashes,
and records the relocation. This was fixed before the successful installed probe.

Compact evidence is tracked in `evidence/2026-09-16-local-validation.json`; full
local evidence is in `artifacts/local/f09-ecapa-installed-probe-2/report.json` and
`/tmp/forum-speaker-runtime-2-check.json`. The runtime has no model weights and
requires explicit host authorization of an existing local model directory.

Remaining acceptance: actual multiple speakers, same-speaker/different-speaker
thresholds, silence/noise/music, short utterances, interruptions and overlap,
meeting-long performance during live ASR/translation, clean second Mac, app
Developer ID signing/notarization. ECAPA does not itself detect overlap; the
worker only respects an explicit host overlap hint. A generated waveform cannot
establish diarization or anonymous-label correctness.
