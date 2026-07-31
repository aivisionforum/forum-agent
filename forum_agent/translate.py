"""Segment translation via the local MLX LLM server. zh/mixed -> English, en -> Chinese."""
from pathlib import Path

import requests

from forum_agent import llm
from forum_agent.constants import (LANG_EN, TRANSLATE_MODEL,
                                   TRANSLATE_TIMEOUT_SECONDS)

_PROMPT = Path(__file__).resolve().parent.parent.joinpath(
    "prompts/translate.txt").read_text()


def target_language(lang: str) -> str:
    return "Simplified Chinese" if lang == LANG_EN else "English"


def translate(text: str, lang: str) -> str:
    """Returns the translation, or '' on any failure (subtitle page degrades
    to ASR-only per spec, so an empty translation is the correct fallback)."""
    prompt = _PROMPT.format(target_language=target_language(lang), text=text)
    try:
        return llm.chat(TRANSLATE_MODEL, prompt, max_tokens=512,
                        timeout=TRANSLATE_TIMEOUT_SECONDS)
    except (requests.RequestException, KeyError) as exc:
        print(f"[translate] degraded to ASR-only: {exc}")
        return ""
