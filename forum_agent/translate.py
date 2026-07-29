"""Segment translation via local Ollama. zh/mixed -> English, en -> Chinese."""
from pathlib import Path

import requests

from forum_agent.constants import (LANG_EN, OLLAMA_MODEL, OLLAMA_URL,
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
        resp = requests.post(OLLAMA_URL, json={
            "model": OLLAMA_MODEL,
            "messages": [{"role": "user", "content": prompt}],
            "think": False,
            "stream": False,
            "options": {"temperature": 0.2},
        }, timeout=TRANSLATE_TIMEOUT_SECONDS)
        resp.raise_for_status()
        return resp.json()["message"]["content"].strip()
    except (requests.RequestException, KeyError) as exc:
        print(f"[translate] degraded to ASR-only: {exc}")
        return ""
