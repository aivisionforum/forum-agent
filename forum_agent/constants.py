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
SILERO_SPEECH_PROB = 0.3
SILERO_MIN_PEAK = 1e-3      # below this post-AGC peak, skip neural VAD
AGC_TARGET_PEAK = 0.5        # auto-gain target for mic input
AGC_MAX_GAIN = 50.0          # cap so pure silence is not amplified into noise
AGC_DECAY = 0.999            # running-peak decay per frame (~2 min half-life)
DEAD_STREAM_SECONDS = 5.0    # all-zero input for this long -> reopen device     # frame is speech if any chunk >= this prob
PRE_ROLL_SECONDS = 0.5       # audio kept from before speech onset (mic)
PEAK_NORM_TARGET = 0.9       # amplify quiet mic segments before Whisper

# Diarization
SPEAKER_SIM_THRESHOLD = 0.72
MAX_SPEAKERS = 8
MIN_EMBED_SECONDS = 0.8

# Translation / Ollama
OLLAMA_URL = "http://localhost:11434/api/chat"
# qwen3:8b chosen over qwen2.5:32b: on a single shared GPU the 32b model's
# ~10x longer decode starves mlx-whisper and subtitle lag blows the 3s budget.
# See README "Model choices". Use 32b only if translation runs on another box.
OLLAMA_MODEL = "qwen3:8b"
TRANSLATE_TIMEOUT_SECONDS = 20

# Insight engine (C4/C6). qwen3:8b w/ thinking: quality is adequate and the
# short-model decode does not starve Whisper on the shared GPU (the 32b scar,
# see ADR 0001). On a two-machine setup, switch to qwen2.5:32b.
INSIGHT_MODEL = "qwen3:8b"
INSIGHT_THINK = True
INSIGHT_INTERVAL_SECONDS = 240   # auto-refresh cadence (spec: every 3-5 min)
INSIGHT_WINDOW_SECONDS = 900     # transcript window fed per refresh
INSIGHT_TIMEOUT_SECONDS = 180
# Review mode: True = auto-approve new items, human corrects/hides on review
# (human-on-the-loop); False = every item is DRAFT until approved (gatekeeper
# mode, for high-stakes projector use). Toggle live on the console.
AUTO_APPROVE_DEFAULT = True
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
WS_PATH = "/ws/room/{room}"
DEFAULT_ROOM = "room1"

# Message types on the websocket
MSG_PARTIAL = "partial"
MSG_FINAL = "final"
MSG_TRANSLATION = "translation"

# Files
SESSIONS_DIR = "data/sessions"   # archived per-session files
TRANSCRIPT_JSONL = "data/{room}_transcript.jsonl"
RECORDING_WAV = "data/{room}_recording.wav"  # raw session audio (C3)
FIXTURE_WAV = "data/fixture_meeting.wav"
FIXTURE_REF_JSON = "data/fixture_reference.json"
