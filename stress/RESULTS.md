# Measured results

First full run of the harness. Everything below came from `stress.run` and
`stress.faults` on the fixture plus modelled room conditions.

**Machine**: MacBook Pro, Apple M3, 16 GB, macOS 26.6.2, Python 3.12.6.
This is *not* the M3 Max / 128 GB the project benchmarked on, which matters
for the latency numbers and nothing else.

**Scope**: 18 pipeline runs, ~73 minutes of audio replayed in real time,
14 fault checks. Commit `1903bb3`.

> The venue was unavailable, so its acoustics are **modelled, not recorded**.
> What transfers is the shape of the degradation — which stage breaks first
> and in what order. Absolute thresholds will differ at the venue. Replace
> `acoustics.hvac()` with a real empty-room recording and these numbers
> become venue-accurate.

## Headline: the VAD latches on

`SileroSpeech.__call__` passed the frame's 512-sample chunks to the model as
a batch. Silero is a streaming RNN that carries hidden state between calls
and expects one chunk at a time; given a batch it treats the rows as
parallel streams sharing a single state, and the state latches once speech
has excited it.

Inter-turn gaps classified as speech, fixture + HVAC noise at 12 dB SNR:

| feeding | gaps read as speech |
|---|---|
| batched (as shipped) | 100.0% |
| `reset_states()` per frame | 0.0% |
| sequential chunks | 4.7% |

With the VAD stuck on, `Segmenter` never accumulates `VAD_SILENCE_SECONDS`,
so no turn is ever closed by the VAD and every utterance is force-closed at
`MAX_SEGMENT_SECONDS`. Twelve-second blocks span speaker changes, so
diarization, language detection and lag all degrade together.

The clean fixture hides this completely — with digital silence between turns
the state decays on its own, which is why the M1 acceptance run passes.

## Front-end comparison

Same audio, same conditions, three front ends:

- **energy** — `run_replay` as shipped: `Segmenter()` with no `speech_fn`
- **neural-batched** — the mic path as shipped: auto-gain + batched Silero
- **neural-sequential** — the mic path with chunks fed one at a time

| condition | front | segs | 12 s closes | CER 中文 | WER en | purity | speakers |
|---|---|---|---|---|---|---|---|
| clean | energy | 20 | 0 | 6.3% | 11.8% | 1.000 | 4/4 |
| clean | neural-sequential | 19 | 0 | 6.3% | 1.1% | 1.000 | 4/4 |
| noise12 | energy | 15 | 14 | 62.8% | 22.9% | 0.697 | 5/4 |
| noise12 | neural-batched | 15 | 14 | 62.8% | 49.5% | 0.697 | 5/4 |
| noise12 | neural-sequential | 19 | 0 | **7.1%** | **1.1%** | **1.000** | **4/4** |
| babble | energy | 15 | 14 | 86.0% | 49.5% | 0.734 | 5/4 |
| babble | neural-batched | 15 | 14 | 85.1% | 49.5% | 0.734 | 5/4 |
| babble | neural-sequential | 19 | 2 | **32.9%** | **22.0%** | **0.958** | **4/4** |

Sequential feeding costs no measurable latency: max subtitle lag 4.61 s
versus 4.79 s on clean audio.

## Condition matrix

Shipped replay path (energy VAD), 170 s per condition unless noted. Content
CER/WER score each emitted segment against the joined text of the reference
utterances it covers, so merged turns are not charged to ASR error.

| condition | segs | 12 s closes | CER 中文 | WER en | purity | spk | max lag | mid-sentence |
|---|---|---|---|---|---|---|---|---|
| clean (control) | 20 | 0 | 6.3% | 11.8% | 1.000 | 4/4 | 4.79 s | 0% |
| reverb RT60 0.6 s | 19 | 0 | 13.3% | 2.1% | 1.000 | 4/4 | 4.89 s | 21% |
| far-field −18 dB | 24 | 0 | 16.0% | 26.7% | 1.000 | 7/4 | 4.80 s | 17% |
| 15 voices | 20 | 1 | 23.1% | 1.1% | 1.000 | 8/15 | 10.16 s | 20% |
| monologue | 15 | 15 | 36.4% | — | 1.000 | 1/1 | 4.86 s | 80% |
| overlap ×3 | 18 | 6 | 38.3% | 1.5% | 0.768 | 6/4 | 14.67 s | 11% |
| HVAC 20 dB SNR | 15 | 13 | 59.5% | 28.9% | 0.796 | 4/4 | 4.79 s | 27% |
| HVAC 6 dB SNR | 15 | 14 | 62.1% | 29.5% | 0.697 | 5/4 | 8.86 s | 40% |
| HVAC 12 dB SNR | 15 | 14 | 62.8% | 22.9% | 0.697 | 5/4 | 8.29 s | 40% |
| audience babble | 15 | 14 | 86.0% | 49.5% | 0.734 | 5/4 | 14.31 s | 40% |
| silence (600 s) | 50 | — | — | — | — | — | 12.47 s | — |

### Reverberation is not the problem

A 0.6 s RT60 — an ordinary hard-surfaced meeting room — costs 13.3% CER with
no force-closes and perfect diarization. The noise floor is what hurts.

### The energy VAD gates on level, not SNR

Far-field at 12 dB SNR scores 16.0% CER while HVAC at 12.5 dB SNR scores
62.8%. `VAD_ENERGY_THRESHOLD = 1e-4` mean-square is −40 dBFS absolute, so
far-field audio (attenuated before noise was added) slips under the gate
while louder noisy audio does not. Auto-gain on the mic path amplifies a
venue feed straight through that threshold.

### Interruption is an independent failure

A third of turns starting 1.8 s early, with no noise at all, gives 38.3% CER
and six labels for four people. The segmenter is single-stream, so
overlapping speech cannot be represented — `tests/acceptance_m1.py` asserts
non-overlapping times.

### Speaker capacity

`MAX_SPEAKERS = 8` is a hard cap and centroids merge but never split. Fifteen
voices produced eight labels, with one label carrying five different
speakers. Note that `diar_purity` reads 1.000 here and is misleading: purity
cannot see merging, only splitting. `merge_max` is the metric to read.

### Silence

Ten minutes of room tone with no speech:

| front | segments | hallucinated chars/min |
|---|---|---|
| energy (shipped replay) | 50 | 80.4 |
| neural-batched | 0 | 0.0 |
| neural-sequential | 1 | 0.1 |

The energy path emitted "Thank you." 32 times, plus word salad ("Open soup
microwave microwave tripod Screen菜aleb…"), and the translator rendered each
one for the projector. Whisper's `compression_ratio` and `no_speech_prob`
guards caught none of it.

The single false positive under sequential feeding is one replacement
character in a 1.25 s segment; a minimum-segment-duration guard would
remove it.

## Latency

The project's recorded evidence (`evidence/replay_stats.json`) is mean 0.44 s,
max 2.68 s on an M3 Max / 128 GB. On this 16 GB M3 the identical clean
fixture gives **mean 3.25 s, max 4.79 s** — over the 3 s acceptance budget
before any room condition is applied. Babble and overlap reach 14.7 s.

Nothing degrades gracefully when inference falls behind wall-clock: the
replay clock keeps advancing and subtitles simply arrive late. Whatever
machine runs the venue needs re-benchmarking, and the 32B report model
(~18 GB in 4-bit) will not fit alongside Whisper and an 8B translator on
16 GB.

## Fault injection

14 checks, 12 ok, 2 risk (`stress/faults.py`).

Held up: room and session traversal rejected with 400; hostile-origin
websocket refused; cross-origin POST 403; double Start does not run two
pipelines onto one transcript; Stop mid-inference archived cleanly in 3 s;
a nonexistent mic device surfaces `PortAudioError` on the console rather
than hanging; ten websocket clients with one dropping mid-session changed
nothing.

**Risk — a dead model server never comes back.** Kill `mlx_lm` mid-session
and the transcript continues while translations silently become empty
strings. The console does render "⚠ LLM server not responding"
(`control.html:106`), so an operator watching it will know, but
`llm.launch_server()` runs once at startup and nothing relaunches it.
Recovery means restarting the application mid-forum.

**Rough edge — `/api/report` returns 500 where its siblings return 409.**
`api_report` lets `RuntimeError` escape; `/api/minutes` and
`/api/insights/run` map it to a 409. Asking for a report over sessions with
no material gives the operator *Internal Server Error*.

## What this cannot tell you

- **Synthetic noise.** HVAC is filtered pink noise plus mains hum; babble is
  layered TTS. A real empty-room recording replaces one function.
- **TTS voices, not people.** No real accents, no vocal fry, no
  Cantonese-influenced Mandarin, no speakers who trail off.
- **No PA in the loop.** No amplification, feedback, or mixer feed. Note
  that `_pick_device` prefers the built-in MacBook microphone when no device
  is chosen explicitly.
- **Short runs.** Longest was ten minutes. A 90-minute session's memory
  growth, thermal throttling and unbounded `approved_log` are untested.
- **The report path never ran.** The 32B model was never downloaded, so
  offline readiness end to end is unverified.
- **Chatham House is narrower than it looks.** Speaker labels are
  anonymised and the prompts forbid names, but the stored transcript
  contains every name spoken aloud, in plaintext, beside a WAV of the room.
