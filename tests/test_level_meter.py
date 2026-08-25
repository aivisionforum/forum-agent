"""Issue #7: /api/status carries the mic level only while it is live and
fresh — a wedged capture thread must not show a frozen 'healthy' meter."""
import time

from forum_agent.session import MODE_MIC, MODE_REPLAY, SessionManager


class _AliveThread:
    def is_alive(self):
        return True


def _running_mic_manager() -> SessionManager:
    m = SessionManager()
    m.mode = MODE_MIC
    m._thread = _AliveThread()
    return m


def test_fresh_mic_level_is_reported():
    m = _running_mic_manager()
    m.level = {"peak": 0.4, "rms": 0.1, "ts": time.time()}
    assert m.status()["level"]["peak"] == 0.4


def test_stale_level_is_suppressed():
    m = _running_mic_manager()
    m.level = {"peak": 0.4, "rms": 0.1, "ts": time.time() - 10}
    assert m.status()["level"] is None


def test_no_level_when_idle_or_replay():
    assert SessionManager().status()["level"] is None
    m = _running_mic_manager()
    m.mode = MODE_REPLAY
    m.level = {"peak": 0.4, "rms": 0.1, "ts": time.time()}
    assert m.status()["level"] is None
