# Room-condition stress harness

The M1 acceptance run measures the pipeline on `data/fixture_meeting.wav`:
close-mic TTS, one voice at a time, 0.9 s of digital silence between turns,
four speakers, no noise, no reverb. A forum room is none of those things.

This harness replays the same speech under modelled room conditions and
scores the result against ground truth, so a regression in noise robustness
shows up as a number instead of a surprise on the projector.

See [RESULTS.md](RESULTS.md) for the first full run's findings.

## The one thing to know first

The venue was unavailable when this was written, so its acoustics are
**modelled, not recorded**. HVAC is filtered pink noise plus mains hum;
audience babble is layered TTS; the room impulse response is synthetic.

Absolute thresholds will differ at the venue. What transfers is the *shape*
of the degradation — which stage breaks first, and in what order.

**Replacing the model with the real thing is one function.** Record fifteen
minutes of the empty room, then return it from `acoustics.hvac()` (or pass
it into `mix_at_snr` directly) and every number the harness produces becomes
venue-accurate. That recording is the highest-value thing anyone attending a
site visit can bring back.

## Requirements

Everything in `requirements.txt`, plus:

- **scipy** — used by `acoustics.py` for `fftconvolve` and `lfilter`. Not
  listed in `requirements.txt` because `scikit-learn` already requires it;
  if that pin is ever dropped, add scipy explicitly.
- **macOS `say` and `ffmpeg`** — only for the `voices15` condition, which is
  the one fixture that needs new synthesis. Every other condition is built
  by slicing the existing fixture.

## Quickstart

```bash
# 1. build fixtures into data/stress/  (fast, except voices15)
.venv/bin/python -m stress.build

# 2. replay each condition through the real pipeline and score it
.venv/bin/python -m stress.run

# 3. re-score without replaying (transcripts persist)
.venv/bin/python -m stress.metrics noise12
```

`stress.run` is resumable: finished conditions are cached in
`data/stress/results.json` and skipped, and `--redo` re-runs only the
conditions named with `--only`. Budget roughly the audio duration in
wall-clock time — replay is pinned to the clock on purpose, because the
whole point is to measure a real-time system in real time. A full matrix is
about 40 minutes.

Fault injection needs the app running:

```bash
.venv/bin/python -m forum_agent.server &
.venv/bin/python -m stress.faults        # writes data/stress/faults.json
```

## Two front ends

`--front` selects how audio reaches the segmenter:

| front | what it exercises |
|---|---|
| `energy` (default) | `replay.run_replay` as shipped — `Segmenter()` with no `speech_fn`, i.e. the energy VAD |
| `neural` | the **mic** path — auto-gain into the Silero neural VAD, fed from a file |

Both matter, because they are not the same code path. `run_mic` uses Silero;
`run_replay` uses the energy VAD at `VAD_ENERGY_THRESHOLD = 1e-4`
mean-square, which is −40 dBFS *absolute* rather than relative to the
speech. Any recording whose room tone sits above that line is treated as one
continuous utterance. The `neural` front exists so the path that will
actually run at the venue can be tested repeatably without a microphone in
the loop.

## Conditions

Every condition is composed from the same speech, sliced out of
`data/fixture_meeting.wav` using the timings in
`data/fixture_reference.json`. A difference in results is therefore caused
by the condition, not by different words.

| name | seconds | what it stresses |
|---|---|---|
| `clean` | 170 | control: the dry fixture, as the acceptance run sees it |
| `noise20` / `noise12` / `noise06` | 170 | HVAC noise at 20, 12 and 6 dB SNR (verified after mixing) |
| `reverb` | 170 | RT60 0.6 s — an ordinary hard-surfaced meeting room |
| `farfield` | 170 | RT60 0.8 s, −18 dB level, 12 dB SNR: the realistic venue case |
| `babble` | 170 | audience murmur bed at 10 dB SNR — speech-shaped, so the hardest case for a neural VAD |
| `overlap` | 161 | every 3rd turn starts 1.8 s before the previous one ends |
| `monologue` | 181 | one speaker, 0.25 s pauses only — below `VAD_SILENCE_SECONDS`, so turns can close only at `MAX_SEGMENT_SECONDS` |
| `voices15` | 190 | 15 distinct voices against `MAX_SPEAKERS = 8` |
| `silence` | 600 | ten minutes of room tone, no speech: **any** text is a false positive |

Two construction details worth knowing if you extend this:

- The synthetic RIR keeps the direct path at sample 0 with unit gain, so
  convolution does not shift onsets and ground-truth timings survive.
- `base_excerpt` cuts at the end of the last *whole* utterance. Cutting at a
  round number leaves a clipped utterance with no ground truth, which then
  scores as hallucinated speech.

## Metrics

`stress.metrics` writes one row per run. The ones that decide whether
subtitles are usable in a room:

| metric | meaning |
|---|---|
| `content_cer_zh` / `content_wer_en` | ASR accuracy, scoring each **segment** against the joined text of the reference utterances it covers |
| `cer_zh` / `wer_en` | the same, pairing one reference to one segment |
| `coverage` | reference utterances that produced a segment overlapping more than half of them |
| `halluc_chars_per_min` | characters emitted where the reference has no speech, over the audio duration |
| `forced_closes` | segments at or above 11.5 s, i.e. closed by `MAX_SEGMENT_SECONDS` rather than by the VAD |
| `frag_rate` | segments not ending on sentence-final punctuation — the subtitle-chopped-mid-sentence rate |
| `diar_purity` | per-true-speaker label consistency, duration-weighted |
| `merge_max` | most distinct true speakers sharing one predicted label |
| `lang_flips` | segments whose detected language sends the translation the wrong way |
| `translation_drop` | segments with no translation, or an empty one |
| `max_lag_s` | worst end-of-utterance to subtitle delay; the acceptance budget is 3 s |
| `peak_rss_gb` | resident memory across the pipeline processes |

Both CER/WER framings are reported on purpose. When turns never close, one
12-second segment spans several reference utterances, and pairing one
reference to one segment charges the whole difference to ASR error. Scoring
each segment against everything it covers separates *wrong words* from
*wrong segment boundaries* — both matter, for different reasons.

Two traps, learned the hard way:

- **`diar_purity` cannot see merging.** If two people always receive the
  same label, each of them is perfectly "pure". Read `merge_max` alongside
  it, and compare `pred_speakers` with `true_speakers`.
- **Per-minute rates need the audio duration**, not the span of the
  segments. One short false positive in a silent file otherwise reads as a
  torrent.

## Fault injection

`stress.faults` answers one question per check: when this goes wrong at the
venue, does the operator *see* it, or does the session quietly degrade?
Verdicts are `ok` (handled and visible), `risk` (handled but silent, or
recovery needs an operator who knows the trick) and `bug`.

It covers path traversal and cross-origin rejection, a double Start click,
Stop during inference, a nonexistent mic device, ten websocket clients with
one dropping mid-session, and the model server being killed mid-session.

Nothing in it fills the disk or opens the microphone for real capture.
