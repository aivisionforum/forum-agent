"""Session manager: lets the web control page start/stop the pipeline with
either the microphone or a test audio file as the source."""
import threading

from forum_agent import replay
from forum_agent.constants import FIXTURE_WAV

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
        engine(room).start_auto()  # C4: auto-refresh while session runs

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
        engine(self.room).stop_auto()
        self._stop.set()
        if self._thread is not None:
            self._thread.join(timeout=90)  # models may be mid-inference
        self._thread = None
        self.mode = MODE_IDLE
        self.phase = ""
        return self.status()


manager = SessionManager()
