# ADR 0001: M1 stack — MLX Whisper + ECAPA clustering + Ollama Qwen

Date: 2026-07-24. Status: accepted.

## Context
M1 needs a fully-local single-room pipeline on an M3 Max (128 GB): replayed
audio → diarized bilingual (zh/en code-switching) transcript → live subtitle
page, with subtitle-usable output < 3 s behind the audio.

## Decisions
1. **ASR: `mlx-whisper` with `whisper-large-v3-turbo`.** MLX runs on Apple
   GPU; a ≤12 s utterance transcribes in well under 1 s, giving measured
   end-of-utterance→subtitle lag ≤ 0.5 s and ~2 s partials. Whisper
   transcribes embedded English inside Chinese utterances verbatim, which is
   exactly what the subtitle display needs. FunASR/Paraformer was considered
   (strong zh+en) but adds a heavy runtime; revisit at M4 if real-room
   accuracy demands it.
2. **Diarization: energy-VAD utterance segmentation + ECAPA-TDNN embeddings
   with online centroid clustering** (threshold 0.72, tuned on measured
   intra ≥ 0.81 / inter ≤ 0.63 cosine on the fixture; 71/71 correct).
   pyannote was rejected for M1: gated download + heavier pipeline.
3. **Translation: Ollama `qwen3:8b` (think=false)**, async so subtitles
   never wait (spec: degrade to ASR-only). `qwen2.5:32b` translated slightly
   better in isolation (~1.6 s/segment warm, fewer untranslated fragments),
   but on the single shared GPU its decode starved mlx-whisper: a full
   replay fell ~1.5x behind real time and subtitle lag blew the 3 s budget.
   8b translates in ~0.5 s and keeps max end-to-end lag at 0.4 s. Revisit
   with a second machine or per-process GPU budgeting at M3/M4.
4. **Plain FastAPI + one WebSocket + a static HTML page**, single process,
   one-command startup — boring and debuggable per the project brief.

## Consequences
- VAD is energy-based; overlapping speech and noisy rooms will need a real
  VAD (e.g. silero) and possibly FunASR before M4 live-mixer work.
- Diarization threshold was tuned on synthetic TTS voices; must be
  re-validated on real recordings.
