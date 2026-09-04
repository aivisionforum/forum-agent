"""Optional cloud-model polish for post-event production (issue #15).

The live forum is fully local. AFTER the event the operator may explicitly
send one draft (minutes or report) to a cloud model for a quality polish.
Credentials never live in the repo. They come from either:
  - environment variables (OPENROUTER_API_KEY, OPENROUTER_MODEL,
    OLLAMA_HOST) — these take precedence, or
  - the console settings form, stored in data/cloud_config.json
    (data/ is wholesale gitignored; the file is chmod 600).
The Ollama provider appears when the daemon has models tagged "-cloud".
The claude / codex providers run the locally installed Claude Code and
Codex CLIs (the operator's own subscriptions — no API key), in one-shot
print mode with no tool access implied by the prompt.

Run the names check first: what you send is what was said aloud."""
import json
import os
import shutil
import subprocess
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


def _cli_path(name: str) -> str | None:
    """Find a subscription CLI: PATH first, then the common install spots
    (the launcher inherits a login shell, but be forgiving)."""
    found = shutil.which(name)
    if found:
        return found
    for cand in (Path.home() / ".local/bin" / name,
                 Path("/opt/homebrew/bin") / name,
                 Path("/usr/local/bin") / name):
        if cand.exists():
            return str(cand)
    return None


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
    if _cli_path("claude"):
        out.append({"id": "claude", "models": CLAUDE_MODELS})
    if _cli_path("codex"):
        out.append({"id": "codex", "models": CODEX_MODELS})
    return out


# Subscription CLIs: "default" uses whatever the CLI is configured with.
CLAUDE_MODELS = ["default", "opus", "sonnet", "haiku"]
CODEX_MODELS = ["default"]


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
    elif provider == "claude":
        models = CLAUDE_MODELS
    elif provider == "codex":
        models = CODEX_MODELS
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
    elif provider in ("claude", "codex"):
        return _chat_cli(provider, model, prompt)
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


TRANSCRIPT_BLOCK = (
    "You are ALSO given the full verbatim meeting transcript below. Use it "
    "to correct errors, restore missing substance, and improve the draft — "
    "but preserve Chatham House anonymity strictly: speakers stay "
    "发言人A/Speaker A etc., and replace any personal name that appears in "
    "speech with a neutral role description.\n\nTranscript:\n{transcript}"
    "\n\n")


def polish_file(src: Path, dest: Path, provider: str, model: str,
                meta: dict | None = None, transcript: str = "") -> Path:
    """Send one local draft to the chosen cloud model. Writes `dest` (the
    'latest' the UI links to) AND an archived copy per run — different
    models produce different polishes worth comparing — plus a sidecar
    json (provider, model, time, extra meta). Never touches the draft."""
    import re as _re
    import time as _time
    if not src.exists():
        raise RuntimeError(f"no draft to polish: {src}")
    from forum_agent import activity
    tblock = (TRANSCRIPT_BLOCK.replace("{transcript}", transcript)
              if transcript else "")
    prompt = (_PROMPT.replace("{transcript_block}", tblock)
              .replace("{draft}", src.read_text()))
    with activity.task(f"polishing with {provider}:{model} (cloud)"):
        text = _chat(provider, model, prompt)
    content = ("> POLISHED DRAFT — produced with a CLOUD model "
               f"({provider}: {model}); still pending human review. "
               "云端模型润色稿，仍需人工确认。\n\n" + text.strip() + "\n")
    dest.write_text(content)
    ts = _time.strftime("%Y%m%d-%H%M%S")
    slug = _re.sub(r"[^A-Za-z0-9]+", "-", f"{provider}-{model}").strip("-")
    arch = dest.with_name(f"{dest.stem}_{ts}_{slug}{dest.suffix}")
    arch.write_text(content)
    arch.with_suffix(".json").write_text(json.dumps(
        {"file": arch.name, "provider": provider, "model": model,
         "generated": ts, **(meta or {})}, ensure_ascii=False))
    return dest


def _chat_cli(provider: str, model: str, prompt: str) -> str:
    """One-shot generation through the operator's own subscription CLI.
    Text goes to Anthropic/OpenAI under that account — post-event use only,
    same warning as every cloud provider."""
    path = _cli_path(provider)
    if not path:
        raise RuntimeError(f"{provider} CLI not found on this machine")
    if provider == "claude":
        cmd = [path, "-p"]
        if model and model != "default":
            cmd += ["--model", model]
        proc = subprocess.run(cmd, input=prompt, capture_output=True,
                              text=True, timeout=POLISH_TIMEOUT * 2)
    else:  # codex
        cmd = [path, "exec", prompt]
        proc = subprocess.run(cmd, capture_output=True, text=True,
                              timeout=POLISH_TIMEOUT * 2)
    if proc.returncode != 0 or not proc.stdout.strip():
        raise RuntimeError(
            f"{provider} CLI failed (exit {proc.returncode}): "
            f"{(proc.stderr or proc.stdout)[-300:]}")
    return proc.stdout.strip()
