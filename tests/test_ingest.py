"""Issue #8: uploaded recordings batch-process into a normal archived
session; a live session blocks ingest."""
import json

import pytest

from forum_agent import session as sess
from forum_agent.session import SessionManager


class _AliveThread:
    def is_alive(self):
        return True


@pytest.fixture
def quiet_pipeline(monkeypatch, tmp_path):
    """Stub the heavy pipeline and the hub; archive into tmp_path."""
    monkeypatch.setattr(sess.replay, "run_replay",
                        lambda *a, **k: None)
    monkeypatch.setattr(sess, "SESSIONS_DIR", str(tmp_path))
    import forum_agent.insights as insights
    import forum_agent.server as server
    monkeypatch.setattr(insights, "engine",
                        lambda room: type("E", (), {"reset": lambda s: None})())
    monkeypatch.setattr(server.hub, "broadcast_from_thread",
                        lambda *a, **k: None)
    return tmp_path


def test_ingest_archives_with_upload_title(quiet_pipeline, monkeypatch):
    calls = []
    sid_dir = quiet_pipeline / "20260102-090000"
    sid_dir.mkdir()

    def fake_archive(room):
        calls.append(room)
        return "20260102-090000" if len(calls) > 1 else None

    monkeypatch.setattr(sess, "archive_live", fake_archive)
    m = SessionManager()
    sid = m.ingest("/tmp/x.wav", "room1", title="paris_keynote.m4a")
    assert sid == "20260102-090000"
    meta = json.loads((sid_dir / "meta.json").read_text())
    assert meta["title"] == "paris_keynote.m4a (uploaded)"
    assert m.status()["running"] is False and m.last_session_id == sid


def test_ingest_refused_while_running(quiet_pipeline):
    m = SessionManager()
    m._thread = _AliveThread()
    with pytest.raises(RuntimeError):
        m.ingest("/tmp/x.wav", "room1", title="x")


def test_silent_upload_returns_none(quiet_pipeline, monkeypatch):
    monkeypatch.setattr(sess, "archive_live", lambda room: None)
    m = SessionManager()
    assert m.ingest("/tmp/x.wav", "room1", title="x") is None
