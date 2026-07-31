"""Single client for all local LLM calls, via the managed mlx-lm server
(OpenAI-compatible chat API). Qwen3 models emit <think> blocks; this client
disables thinking unless asked and strips any block that leaks through."""
import re
import subprocess
import sys
import time

import requests

from forum_agent.constants import CHAT_URL, MLX_SERVER_PORT

_THINK_RE = re.compile(r"<think>.*?</think>\s*", re.S)


def chat(model: str, prompt: str, temperature: float = 0.2,
         max_tokens: int = 2048, timeout: float = 120,
         think: bool = False) -> str:
    """One chat completion. Raises requests exceptions on failure — callers
    decide whether to degrade (translation) or surface (insights/report)."""
    if "Qwen3" in model and not think:
        prompt = prompt + " /no_think"
    last_exc: Exception = RuntimeError("unreachable")
    for attempt in range(5):
        try:
            resp = requests.post(CHAT_URL, json={
                "model": model,
                "messages": [{"role": "user", "content": prompt}],
                "temperature": temperature, "max_tokens": max_tokens,
            }, timeout=timeout)
            resp.raise_for_status()
            # think-mode responses put reasoning in a separate field and can
            # omit content entirely if max_tokens ran out mid-think
            text = resp.json()["choices"][0]["message"].get("content") or ""
            return _THINK_RE.sub("", text).strip()
        except (requests.ConnectionError, requests.Timeout,
                requests.HTTPError) as exc:
            # Transient: the server 404s/errors briefly while switching
            # models (e.g. 32B report -> 8B translation). Retry, then let
            # the caller's policy decide (degrade vs surface).
            status = getattr(getattr(exc, "response", None), "status_code", 0)
            if isinstance(exc, requests.HTTPError) and status not in (404, 503):
                raise
            last_exc = exc
            print(f"[llm] attempt {attempt + 1} failed ({exc}); retrying")
            time.sleep(1.5 * (attempt + 1))  # ~22s total: covers model load
    raise last_exc


def prewarm() -> None:
    """Load the live model right after server start so no user request
    lands in the model-load window (404s cluster there)."""
    from forum_agent.constants import TRANSLATE_MODEL
    try:
        chat(TRANSLATE_MODEL, "hello", max_tokens=4, timeout=300)
        print("[llm] live model warmed")
    except Exception as exc:  # surfaced by /api/status llm health anyway
        print(f"[llm] prewarm failed: {exc!r}")


def healthy(timeout: float = 1.0) -> bool:
    try:
        requests.get(f"http://127.0.0.1:{MLX_SERVER_PORT}/v1/models",
                     timeout=timeout).raise_for_status()
        return True
    except requests.RequestException:
        return False


def launch_server() -> subprocess.Popen | None:
    """Start the mlx-lm server as a managed child (no --model: it loads the
    model named in each request, so translation and report models coexist).
    Returns None if one is already running (e.g. another forum-agent)."""
    if healthy():
        return None
    proc = subprocess.Popen(
        [sys.executable, "-m", "mlx_lm", "server",
         "--port", str(MLX_SERVER_PORT)],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    deadline = time.time() + 30
    while time.time() < deadline:
        if healthy():
            return proc
        time.sleep(0.5)
    proc.terminate()
    raise RuntimeError("mlx-lm server failed to start within 30s")
