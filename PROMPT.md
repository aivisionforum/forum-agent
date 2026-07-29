# Build "Forum Agent" — a live AI participant for AI Vision Forum Shenzhen 2026

Requirement spec (human-facing, EN+中文): https://docs.google.com/document/d/1cG389nV5FcGFuTxlPZ5srfHDaSXSuB_djUKMHieLbTI/edit

Target hardware for development and the PoC: MacBook Pro 16" (Nov 2023), Apple M3 Max, 128 GB RAM, macOS Tahoe. Ollama is installed — use it for all local LLM calls (translation, insights, minutes); pick a Qwen-class instruct model that fits comfortably (e.g. qwen2.5:32b or larger given 128 GB; benchmark and document the choice).

When the PoC (M1) is done and verified, create a new public repo under the GitHub org `aivisionforum` (account `csheargm` has admin; use `gh`), e.g. `aivisionforum/forum-agent`, Apache-2.0 license, and push the code with a proper README (EN + 中文 quickstart).

## Mission

Build an open-source system that acts as an AI participant in a two-day, bilingual (EN/中文), invitation-only forum held under the Chatham House Rule. It listens to live room audio, transcribes and translates in real time, periodically synthesizes the discussion into on-screen insights, produces instant end-of-session minutes, and drafts the post-event synthesis report. It must run **fully locally** (no cloud APIs for audio or text; no internet dependence at the venue) and treat all AI output as **drafts requiring human confirmation** — the system itself demonstrates "human agency": AI amplifies the discussion, humans keep judgment.

## Context and constraints

- Event: Oct 14–15, 2026. Day 1: one room. Day 2 afternoon: **two rooms in parallel**. Audio arrives as line-level feeds from each room's mixing board (assume USB audio interfaces), not room microphones.
- Language: heavy Mandarin/English **code-switching** within single utterances is the norm. Both transcript and display must handle it.
- Privacy: Chatham House Rule. No speaker names anywhere. Diarization labels speakers only as 发言人A/B/Speaker A/B. All storage stays on local disk; encrypted at rest is a plus. Nothing leaves the machines.
- Output review: every AI-generated artifact (insight panel items, minutes, report) is marked DRAFT until a human operator approves it in the operator console.
- License: Apache-2.0 from the first commit. Public repo structure, DCO sign-offs, English README with 中文 quickstart. No proprietary dependencies.

## Components to build

1. **Ingest + ASR service.** Continuous capture from a named audio device → streaming speech-to-text with timestamps and diarization. Evaluate local Whisper (large-v3 / distil) with a code-switching strategy vs FunASR/Paraformer (strong zh+en) vs hybrid. Latency budget: subtitle-usable partials < 3s. Emits append-only JSONL per room: `{t_start, t_end, speaker_id, lang, text}`.
2. **Translation layer.** Each final segment translated to the other language (zh→en, en→zh) via Ollama. Subtitles show original + translation.
3. **Subtitle display.** Full-screen web page per room for a projector: rolling bilingual subtitles, large type, high contrast, dark theme, auto-scroll, URL-param config. Degrades to ASR-only if the LLM lags.
4. **Insight engine.** Every N minutes (default 4) per room, feed the last ~15 min of transcript + running state to the LLM with a fixed prompt producing structured JSON: `{summary_points[], emerging_consensus[], tensions[], open_questions[]}`. Maintain session-long running state (cumulative, not amnesiac). Also emit a one-line "what this room is converging on" for the cross-room ticker.
5. **Displays.** (a) Insight panel web page updating live, items tagged DRAFT until operator-approved. (b) Cross-room ticker. (c) Operator console: approve/edit/hide items, correct language routing, mark session start/end, trigger minutes.
6. **Minutes generator.** On session end: structured bilingual draft (key points, conclusions, action items) as Markdown, ready ~5 min after session end.
7. **Report drafter.** Post-event batch job: anonymized bilingual synthesis-report draft from all transcripts + approved insights.
8. **Ops.** One-command startup per room, local persistence (SQLite + JSONL), crash-resume without transcript loss, smoke-test mode replaying a recorded audio file end-to-end, bilingual test fixture (~10 min synthetic code-switching meeting audio via TTS, or documented recording).

## Non-goals (v1)

No speaker identification/names, no cloud fallback, no mobile app, no participant-facing query page, no video.

## Milestones

- **M1 (the PoC)**: single-room pipeline — audio file replay → diarized bilingual transcript → subtitle page. Acceptance: replay a 10-min zh/en mixed recording; subtitles readable, < 3s behind, transcript JSONL complete.
- **M2**: insight engine + operator console + minutes. Acceptance: on the same replay, insight panel updates on schedule with sane structured output; operator can approve/edit; session-end minutes generated in both languages.
- **M3**: two-room mode + cross-room ticker + report drafter. Acceptance: two replay streams simultaneously without interference; final report draft generated from both.
- **M4**: hardening — live mixer input, crash-resume test, 3-hour soak, README + deployment guide (中文 quickstart), rehearsal checklist.

Work milestone by milestone; after each, run its acceptance test and show the evidence before moving on. Prefer boring, debuggable technology (Python/FastAPI or Node, plain WebSockets, SQLite) over frameworks. Every LLM prompt lives in a versioned `prompts/` directory — they will be tuned by non-developers.
