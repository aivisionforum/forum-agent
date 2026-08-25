"""Optional cloud-model polish for post-event production (issue #15).

The live forum is fully local. AFTER the event the operator may explicitly
send one draft (minutes or report) to a cloud model for a quality polish.
Credentials come from the environment only — never from the repo or data/:

  OPENROUTER_API_KEY   enables the OpenRouter provider
  OPENROUTER_MODEL     optional, default anthropic/claude-sonnet-4.5
  OLLAMA_HOST          optional, default http://127.0.0.1:11434; the Ollama
                       provider appears when models tagged "-cloud" exist

Run the names check first: what you send is what was said aloud."""
import os
from pathlib import Path

import requests

OPENROUTER_URL = "https://openrouter.ai/api/v1/chat/completions"
DEFAULT_OPENROUTER_MODEL = "anthropic/claude-sonnet-4.5"
POLISH_TIMEOUT = 300

_PROMPT = Path(__file__).resolve().parent.parent.joinpath(
    "prompts/polish.txt").read_text()


def _ollama_host() -> str:
    return os.environ.get("OLLAMA_HOST", "http://127.0.0.1:11434").rstrip("/")


def providers() -> list[dict]:
    """Configured cloud providers, for the console picker. Empty when no
    credentials are set — the UI then shows nothing."""
    out = []
    if os.environ.get("OPENROUTER_API_KEY"):
        out.append({"id": "openrouter",
                    "models": [os.environ.get("OPENROUTER_MODEL",
                                              DEFAULT_OPENROUTER_MODEL)]})
    try:
        tags = requests.get(f"{_ollama_host()}/api/tags", timeout=2).json()
        cloud = [m["name"] for m in tags.get("models", [])
                 if m["name"].endswith("-cloud")]
        if cloud:
            out.append({"id": "ollama", "models": sorted(cloud)})
    except (requests.RequestException, ValueError):
        pass  # no local ollama daemon: provider simply not offered
    return out


def _chat(provider: str, model: str, prompt: str) -> str:
    if provider == "openrouter":
        key = os.environ.get("OPENROUTER_API_KEY")
        if not key:
            raise RuntimeError("OPENROUTER_API_KEY is not set")
        r = requests.post(OPENROUTER_URL, timeout=POLISH_TIMEOUT,
                          headers={"Authorization": f"Bearer {key}"},
                          json={"model": model,
                                "messages": [{"role": "user",
                                              "content": prompt}]})
    elif provider == "ollama":
        r = requests.post(f"{_ollama_host()}/v1/chat/completions",
                          timeout=POLISH_TIMEOUT,
                          json={"model": model,
                                "messages": [{"role": "user",
                                              "content": prompt}]})
    else:
        raise RuntimeError(f"unknown provider: {provider}")
    r.raise_for_status()
    return r.json()["choices"][0]["message"]["content"]


def polish_file(src: Path, dest: Path, provider: str, model: str) -> Path:
    """Send one local draft to the chosen cloud model and write the polished
    version next to it, with a provenance banner. Never touches the draft."""
    if not src.exists():
        raise RuntimeError(f"no draft to polish: {src}")
    from forum_agent import activity
    with activity.task(f"polishing with {provider}:{model} (cloud)"):
        text = _chat(provider, model,
                     _PROMPT.replace("{draft}", src.read_text()))
    dest.write_text(
        "> POLISHED DRAFT — produced with a CLOUD model "
        f"({provider}: {model}); still pending human review. "
        "云端模型润色稿，仍需人工确认。\n\n" + text.strip() + "\n")
    return dest
