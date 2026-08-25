"""Optional cloud-model polish for post-event production (issue #15).

The live forum is fully local. AFTER the event the operator may explicitly
send one draft (minutes or report) to a cloud model for a quality polish.
Credentials never live in the repo. They come from either:
  - environment variables (OPENROUTER_API_KEY, OPENROUTER_MODEL,
    OLLAMA_HOST) — these take precedence, or
  - the console settings form, stored in data/cloud_config.json
    (data/ is wholesale gitignored; the file is chmod 600).
The Ollama provider appears when the daemon has models tagged "-cloud".

Run the names check first: what you send is what was said aloud."""
import json
import os
from pathlib import Path

import requests

from forum_agent.constants import CLOUD_CONFIG_JSON

OPENROUTER_URL = "https://openrouter.ai/api/v1/chat/completions"
DEFAULT_OPENROUTER_MODEL = "anthropic/claude-sonnet-4.5"
POLISH_TIMEOUT = 300

_PROMPT = Path(__file__).resolve().parent.parent.joinpath(
    "prompts/polish.txt").read_text()


def load_config() -> dict:
    """Console-entered settings (data/cloud_config.json, gitignored).
    Environment variables take precedence when both are set."""
    try:
        return json.loads(Path(CLOUD_CONFIG_JSON).read_text())
    except (OSError, ValueError):
        return {}


def save_config(updates: dict) -> dict:
    """Merge console updates into the local config file. An explicit empty
    string clears a field. Returns the sanitized view (never the key)."""
    cfg = load_config()
    for k in ("openrouter_api_key", "openrouter_model", "ollama_host"):
        if k in updates:
            v = str(updates[k]).strip()
            if v:
                cfg[k] = v
            else:
                cfg.pop(k, None)
    path = Path(CLOUD_CONFIG_JSON)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(cfg))
    path.chmod(0o600)  # holds an API key: owner-only
    return config_view()


def config_view() -> dict:
    """Config for the console — reports WHETHER a key is set, never the key."""
    cfg = load_config()
    return {"openrouter_configured": bool(_openrouter_key()),
            "openrouter_key_source": ("env" if os.environ.get(
                "OPENROUTER_API_KEY") else
                "file" if cfg.get("openrouter_api_key") else None),
            "openrouter_model": _openrouter_model(),
            "ollama_host": _ollama_host()}


def _openrouter_key() -> str:
    return (os.environ.get("OPENROUTER_API_KEY")
            or load_config().get("openrouter_api_key", ""))


def _openrouter_model() -> str:
    return (os.environ.get("OPENROUTER_MODEL")
            or load_config().get("openrouter_model",
                                 DEFAULT_OPENROUTER_MODEL))


def _ollama_host() -> str:
    return (os.environ.get("OLLAMA_HOST")
            or load_config().get("ollama_host")
            or "http://127.0.0.1:11434").rstrip("/")


def providers() -> list[dict]:
    """Configured cloud providers, for the console picker. Empty when no
    credentials are set — the UI then shows nothing."""
    out = []
    if _openrouter_key():
        out.append({"id": "openrouter", "models": [_openrouter_model()]})
    try:
        tags = requests.get(f"{_ollama_host()}/api/tags", timeout=2).json()
        cloud = [m["name"] for m in tags.get("models", [])
                 if m["name"].endswith((":cloud", "-cloud"))]
        if cloud:
            out.append({"id": "ollama", "models": sorted(cloud)})
    except (requests.RequestException, ValueError):
        pass  # no local ollama daemon: provider simply not offered
    return out


# Sensible defaults per provider for the polish use case, in preference
# order; the first one present in the live model list is preselected.
PREFERRED = {
    "openrouter": [DEFAULT_OPENROUTER_MODEL, "openai/gpt-5.2",
                   "google/gemini-3-pro", "deepseek/deepseek-v4"],
    "ollama": ["gpt-oss:120b:cloud", "deepseek-v4-pro:0813:cloud",
               "qwen3-max:cloud"],
}


def list_models(provider: str) -> dict:
    """Live model list + preselected default for the console picker."""
    if provider == "openrouter":
        if not _openrouter_key():
            return {"models": [], "default": None}
        r = requests.get("https://openrouter.ai/api/v1/models", timeout=10)
        r.raise_for_status()
        models = sorted(m["id"] for m in r.json().get("data", []))
    elif provider == "ollama":
        tags = requests.get(f"{_ollama_host()}/api/tags", timeout=3).json()
        models = sorted(m["name"] for m in tags.get("models", [])
                        if m["name"].endswith((":cloud", "-cloud")))
    else:
        raise RuntimeError(f"unknown provider: {provider}")
    configured = _openrouter_model() if provider == "openrouter" else None
    default = next((m for m in [configured, *PREFERRED.get(provider, [])]
                    if m in models), models[0] if models else None)
    return {"models": models, "default": default}


def test_model(provider: str, model: str) -> dict:
    """One tiny round trip so the operator can verify a model before using
    it on a real draft."""
    import time
    t0 = time.time()
    reply = _chat(provider, model,
                  "Reply with exactly the two characters: OK")
    return {"ok": "OK" in reply.upper(), "seconds": round(time.time() - t0, 1),
            "reply": reply.strip()[:60]}


def _chat(provider: str, model: str, prompt: str) -> str:
    if provider == "openrouter":
        key = _openrouter_key()
        if not key:
            raise RuntimeError("OpenRouter API key is not configured")
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
