"""Invoked voice summary (issue #14): on demand, the agent reads the current
approved key points aloud through the room speakers — invoked speech only,
so it never interrupts humans. Local TTS via macOS `say`; nothing leaves
the machine."""
import subprocess
import threading

VOICE_ZH = "Tingting"   # macOS built-in Mandarin voice
VOICE_EN = "Samantha"   # macOS built-in English voice
MAX_POINTS = 4          # keep the spoken summary under ~a minute

_speaking = threading.Lock()
_current = {"proc": None, "stop": False}


def stop_speaking() -> None:
    """Cut the voice off mid-sentence (console Stop button)."""
    _current["stop"] = True
    proc = _current["proc"]
    if proc is not None and proc.poll() is None:
        proc.terminate()


def build_script(state: dict) -> list[tuple[str, str]]:
    """(voice, text) chunks from approved key points; bilingual per point."""
    chunks = [(VOICE_ZH, "大家好，我是论坛智能体。以下是目前的要点摘要。"),
              (VOICE_EN, "Hello, this is the Forum Agent. "
                         "Here are the key points so far.")]
    points = [i for i in state.get("items", {}).get("summary_points", [])
              if i.get("status") == "approved"][:MAX_POINTS]
    if not points:
        raise RuntimeError("no approved key points to speak yet")
    for n, it in enumerate(points, 1):
        if it.get("zh"):
            chunks.append((VOICE_ZH, f"第{n}点。{it['zh']}。"))
        if it.get("en"):
            chunks.append((VOICE_EN, f"Point {n}. {it['en']}."))
    line = state.get("convergence_line") or {}
    if line.get("zh"):
        chunks.append((VOICE_ZH, f"目前的整体方向是：{line['zh']}。"))
    return chunks


def speak_summary(state: dict) -> None:
    """Blocking: speak the summary through the default output device.
    Refuses to overlap with itself (one voice at a time)."""
    chunks = build_script(state)  # raises before taking the lock
    if not _speaking.acquire(blocking=False):
        raise RuntimeError("already speaking — wait for it to finish")
    try:
        from forum_agent import activity
        _current["stop"] = False
        with activity.task("speaking summary aloud"):
            for voice, text in chunks:
                if _current["stop"]:
                    break
                proc = subprocess.Popen(["say", "-v", voice, text])
                _current["proc"] = proc
                proc.wait()
    finally:
        _speaking.release()
