"""Session manager: lets the web control page start/stop the pipeline with
either the microphone or a test audio file as the source. Starting a new
session archives the previous one under data/sessions/<timestamp>/ so past
meetings stay loadable."""
import json
import shutil
import threading
import time
from pathlib import Path

from forum_agent import replay
from forum_agent.constants import (FIXTURE_WAV, INSIGHTS_JSON, MINUTES_MD,
                                   MSG_SESSION_RESET,
                                   SESSIONS_DIR, TRANSCRIPT_JSONL)


def archive_live(room: str) -> str | None:
    """Move the current live files into a timestamped session directory.
    Returns the session id, or None if there was nothing to archive."""
    transcript = Path(TRANSCRIPT_JSONL.format(room=room))
    if not transcript.exists() or transcript.stat().st_size == 0:
        return None
    sid = time.strftime("%Y%m%d-%H%M%S", time.localtime(
        transcript.stat().st_mtime))  # named after last activity, not now
    dest = Path(SESSIONS_DIR) / sid
    dest.mkdir(parents=True, exist_ok=True)
    for pattern in (TRANSCRIPT_JSONL, "data/{room}_translations.jsonl",
                    INSIGHTS_JSON, "data/{room}_insights_history.jsonl",
                    "data/{room}_minutes*.md",
                    "data/{room}_recording.wav"):
        tp = Path(pattern.format(room=room))
        for src in tp.parent.glob(tp.name):
            if src.stat().st_size > 0:
                shutil.move(str(src), dest / src.name)
    (dest / "meta.json").write_text(json.dumps(
        {"title": _default_title(dest, room)}, ensure_ascii=False))
    return sid


def _default_title(session_dir: Path, room: str) -> str:
    """Default session title: the convergence line if insights exist, else
    the opening words of the transcript."""
    import re
    ins = session_dir / f"{room}_insights.json"
    try:
        if ins.exists():
            data = json.loads(ins.read_text())
            topic = data.get("session_topic", {})
            if topic.get("zh"):
                return topic["zh"][:40]
            line = data.get("convergence_line", {})
            if line.get("zh"):  # older sessions: strip "the room is..." phrasing
                return re.sub(r"^(该房间|本房间|该会场|本次会议|The room is converging on\s*)(正在)?(聚焦于|讨论|收敛于)?",
                              "", line["zh"]).strip("。 ")[:40]
        t = session_dir / f"{room}_transcript.jsonl"
        first = json.loads(t.read_text().splitlines()[0])
        return first["text"][:40]
    except Exception:
        return "Untitled session"


def rename_session(session_id: str, title: str) -> bool:
    import re
    if not re.fullmatch(r"\d{8}-\d{6}", session_id) or not title.strip():
        return False
    d = Path(SESSIONS_DIR) / session_id
    if not d.is_dir():
        return False
    meta = d / "meta.json"
    data = json.loads(meta.read_text()) if meta.exists() else {}
    data["title"] = title.strip()[:120]
    meta.write_text(json.dumps(data, ensure_ascii=False))
    return True


def delete_session(session_id: str) -> bool:
    """Operator-initiated removal of one archived session (confirmed in UI).
    The id must be a pure timestamp: rejects any path-traversal attempt."""
    import re
    if not re.fullmatch(r"\d{8}-\d{6}", session_id):
        return False
    target = Path(SESSIONS_DIR) / session_id
    if not target.is_dir():
        return False
    shutil.rmtree(target)
    return True


def list_sessions() -> list[dict]:
    root = Path(SESSIONS_DIR)
    if not root.exists():
        return []
    out = []
    for d in sorted(root.iterdir(), reverse=True):
        if d.is_dir():
            t = next(iter(d.glob("*_transcript.jsonl")), None)
            meta = d / "meta.json"
            title = ""
            if meta.exists():
                try:
                    title = json.loads(meta.read_text()).get("title", "")
                except json.JSONDecodeError:
                    pass
            out.append({"id": d.name, "title": title or "Untitled session",
                        "files": sorted(f.name for f in d.iterdir()),
                        "segments": len(t.read_text().splitlines()) if t else 0})
    return out

MODE_MIC = "mic"
MODE_REPLAY = "replay"
MODE_IDLE = "idle"


class SessionManager:
    def __init__(self) -> None:
        self._thread: threading.Thread | None = None
        self._stop = threading.Event()
        self.mode = MODE_IDLE
        self.room = "room1"
        self.phase = ""
        self.error: str | None = None
        self.last_session_id: str | None = None

    def status(self) -> dict:
        running = self._thread is not None and self._thread.is_alive()
        if not running and self.mode != MODE_IDLE:
            self.mode = MODE_IDLE  # session finished on its own
            self.phase = ""
        return {"mode": self.mode, "room": self.room, "running": running,
                "phase": self.phase, "error": self.error}

    def start(self, mode: str, room: str = "room1",
              wav: str = FIXTURE_WAV, play: bool = True,
              device: int | None = None) -> dict:
        self.stop()
        self._stop = threading.Event()
        self.error = None
        stop = self._stop

        from forum_agent.insights import engine
        from forum_agent.server import hub
        archive_live(room)         # previous session -> data/sessions/<id>/
        engine(room).reset()       # fresh state for the new meeting
        engine(room).start_auto()  # C4: auto-refresh while session runs
        # open subtitle/insight pages clear themselves for the new session
        hub.broadcast_from_thread(room, {"type": MSG_SESSION_RESET})

        def _run() -> None:
            try:
                if mode == MODE_MIC:
                    replay.run_mic(room, stop_event=stop, device=device,
                                   on_phase=lambda p: setattr(self, "phase", p))
                else:
                    replay.run_replay(wav, room, play=play, stop_event=stop,
                                      on_phase=lambda p: setattr(self, "phase", p))
            except Exception as exc:  # surfaced via /api/status for the UI
                self.error = f"{type(exc).__name__}: {exc}"
                raise

        self.mode, self.room = mode, room
        self._thread = threading.Thread(target=_run, daemon=True)
        self._thread.start()
        return self.status()

    def stop(self) -> dict:
        from forum_agent.insights import engine
        was_running = self._thread is not None and self._thread.is_alive()
        engine(self.room).stop_auto()
        self._stop.set()
        if self._thread is not None:
            self._thread.join(timeout=90)  # models may be mid-inference
        self._thread = None
        self.mode = MODE_IDLE
        self.phase = ""
        if was_running:  # finished session appears in Past sessions right away
            self.last_session_id = archive_live(self.room)
        return self.status()


manager = SessionManager()
