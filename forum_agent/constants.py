"""Single source of truth for strings/keys shared across modules (R3)."""

# Audio
SAMPLE_RATE = 16000
FRAME_SECONDS = 0.25         # replay feed granularity
VAD_SILENCE_SECONDS = 0.5    # silence gap that closes a segment
VAD_ENERGY_THRESHOLD = 1e-4  # mean-square energy below this = silence
MAX_SEGMENT_SECONDS = 12.0   # force-close very long utterances
PARTIAL_INTERVAL_SECONDS = 2.0

# ASR
WHISPER_MODEL = "mlx-community/whisper-large-v3-turbo"
# Hallucination guards: drop Whisper segments that look like noise-induced
# loops ("old old old ...") or non-speech. Values are Whisper's own defaults.
ASR_MAX_COMPRESSION_RATIO = 2.4
ASR_MAX_NO_SPEECH_PROB = 0.6

# Neural VAD (Silero) for mic input: no calibration or tuning required
SILERO_CHUNK = 512           # samples per Silero inference at 16 kHz
SILERO_SPEECH_PROB = 0.3     # frame is speech if any chunk >= this prob
SILERO_MIN_PEAK = 1e-3      # below this post-AGC peak, skip neural VAD
AGC_TARGET_PEAK = 0.5        # auto-gain target for mic input
AGC_MAX_GAIN = 50.0          # cap so pure silence is not amplified into noise
AGC_DECAY = 0.999            # running-peak decay per frame (~2 min half-life)
DEAD_STREAM_SECONDS = 5.0    # all-zero input for this long -> reopen device
PRE_ROLL_SECONDS = 0.5       # audio kept from before speech onset (mic)
PEAK_NORM_TARGET = 0.9       # amplify quiet mic segments before Whisper

# Diarization
SPEAKER_SIM_THRESHOLD = 0.72
# Afternoon working sessions are roundtables — 20+ people may speak. New
# centroids past the cap are merged into the nearest existing speaker, so a
# low cap silently mislabels; 24 keeps labels honest for a working session.
# Trade-off: with many similar voices, online clustering gets noisier — the
# labels stay anonymous (Speaker A/B/...) so errors cost readability, not
# privacy.
MAX_SPEAKERS = 24
MIN_EMBED_SECONDS = 0.8

# Local LLM serving: mlx-lm server (OpenAI-compatible), launched and managed
# by forum_agent.server itself — nothing to babysit at the venue. MLX
# benchmarked ~1.8x faster generation than Ollama/llama.cpp on this M3 Max
# (46 vs 25 tok/s, qwen3-8b), and shares the framework with mlx-whisper.
MLX_SERVER_PORT = 8711
CHAT_URL = f"http://127.0.0.1:{MLX_SERVER_PORT}/v1/chat/completions"
TRANSLATE_MODEL = "mlx-community/Qwen3-8B-4bit"
TRANSLATE_TIMEOUT_SECONDS = 30

# Insight engine (C4/C6). qwen3:8b w/ thinking: quality is adequate and the
# short-model decode does not starve Whisper on the shared GPU (the 32b scar,
# see ADR 0001). On a two-machine setup, switch to qwen2.5:32b.
INSIGHT_MODEL = TRANSLATE_MODEL  # same 8B: live path stays small (ADR 0001)
INSIGHT_THINK = True
INSIGHT_INTERVAL_SECONDS = 180   # auto-refresh cadence (spec: every 3-5 min)
INSIGHT_WINDOW_SECONDS = 900     # transcript window fed per refresh
INSIGHT_TIMEOUT_SECONDS = 180
# Review mode: True = auto-approve new items, human corrects/hides on review
# (human-on-the-loop); False = every item is DRAFT until approved (gatekeeper
# mode, for high-stakes projector use). Toggle live on the console.
AUTO_APPROVE_DEFAULT = True
INSIGHT_MAX_ITEMS = {"summary_points": 6, "emerging_consensus": 4,
                     "tensions": 4, "open_questions": 4}
# Report drafter (C7): batch job, quality over latency -> the big model is
# fine here (nothing else needs the GPU when the report runs).
REPORT_MODEL = "mlx-community/Qwen2.5-32B-Instruct-4bit"
REPORT_TIMEOUT_SECONDS = 900
REPORT_MD = "data/report_draft.md"

INSIGHTS_JSON = "data/{room}_insights.json"
INSIGHTS_HISTORY = "data/{room}_insights_history.jsonl"  # every refresh, append-only
MINUTES_MD = "data/{room}_minutes.md"
MSG_INSIGHTS = "insights"
MSG_SESSION_RESET = "session_reset"

# Language tags used in JSONL and UI
LANG_ZH = "zh"
LANG_EN = "en"
LANG_MIXED = "mixed"

# Server
SERVER_HOST = "127.0.0.1"
SERVER_PORT = 8710

# LLM watchdog: the mlx-lm server dies silently under memory pressure and
# translations become empty strings (stress/RESULTS.md). The watchdog checks
# health every interval and relaunches after this many consecutive failures.
LLM_WATCHDOG_INTERVAL_SECONDS = 10
LLM_WATCHDOG_FAILURES = 3

# Message types on the websocket
MSG_PARTIAL = "partial"
MSG_FINAL = "final"
MSG_TRANSLATION = "translation"

# Files
SESSIONS_DIR = "data/sessions"   # archived per-session files
TRANSCRIPT_JSONL = "data/{room}_transcript.jsonl"
TRANSLATIONS_JSONL = "data/{room}_translations.jsonl"
RECORDING_WAV = "data/{room}_recording.wav"  # raw session audio (C3)
FIXTURE_WAV = "data/fixture_meeting.wav"
FIXTURE_REF_JSON = "data/fixture_reference.json"
