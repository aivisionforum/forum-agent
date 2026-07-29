"""ASR via mlx-whisper (Apple-silicon optimized). Handles zh/en code-switching
by letting Whisper auto-detect language per utterance; embedded English inside
Chinese utterances is transcribed verbatim by large-v3-turbo."""
import re

import mlx_whisper
import numpy as np

from forum_agent.constants import (ASR_MAX_COMPRESSION_RATIO, PEAK_NORM_TARGET,
                                   ASR_MAX_NO_SPEECH_PROB, LANG_EN,
                                   LANG_MIXED, LANG_ZH, WHISPER_MODEL)

_CJK = re.compile(r"[一-鿿]")
_LATIN = re.compile(r"[A-Za-z]")


def detect_lang(text: str) -> str:
    has_zh, has_en = bool(_CJK.search(text)), bool(_LATIN.search(text))
    if has_zh and has_en:
        return LANG_MIXED
    return LANG_ZH if has_zh else LANG_EN


def transcribe(audio: np.ndarray) -> tuple[str, str]:
    """Returns (text, lang). Empty text if nothing recognized.

    Segments that look like noise-induced hallucinations (repetition loops
    have a high compression ratio; non-speech has a high no-speech
    probability) are dropped rather than shown on the projector."""
    peak = float(np.max(np.abs(audio))) if len(audio) else 0.0
    if 0 < peak < PEAK_NORM_TARGET:  # amplify quiet mic audio (low input gain)
        audio = audio * (PEAK_NORM_TARGET / peak)
    result = mlx_whisper.transcribe(
        audio, path_or_hf_repo=WHISPER_MODEL,
        condition_on_previous_text=False, fp16=True)
    kept = [s["text"] for s in result.get("segments", [])
            if s.get("compression_ratio", 0) <= ASR_MAX_COMPRESSION_RATIO
            and s.get("no_speech_prob", 0) <= ASR_MAX_NO_SPEECH_PROB]
    text = "".join(kept).strip()
    return text, (detect_lang(text) if text else LANG_EN)


def warmup() -> None:
    """Load model weights before the replay clock starts."""
    transcribe(np.zeros(16000, dtype=np.float32))
