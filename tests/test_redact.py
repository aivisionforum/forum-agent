"""Issue #10: the name check writes an advisory report and never edits the
transcript; unparseable model output is an error, not a clean result."""
import json

import pytest

from forum_agent import redact


def _make_session(tmp_path, monkeypatch, texts):
    monkeypatch.setattr(redact, "SESSIONS_DIR", str(tmp_path))
    sdir = tmp_path / "20260101-100000"
    sdir.mkdir()
    (sdir / "room1_transcript.jsonl").write_text(
        "\n".join(json.dumps({"text": t}) for t in texts))
    return sdir


def test_report_lists_names_with_lines(tmp_path, monkeypatch):
    sdir = _make_session(tmp_path, monkeypatch,
                         ["hello from Alice", "open source", "Alice agrees"])
    before = (sdir / "room1_transcript.jsonl").read_text()
    monkeypatch.setattr(redact.llm, "chat", lambda *a, **k:
                        '{"names": [{"name": "Alice", "lines": [0, 2]}]}')
    report = redact.check_session(sdir.name)
    text = report.read_text()
    assert "Alice" in text and "line 0" in text and "line 2" in text
    assert "DRAFT" in text
    # advisory only: the transcript itself is untouched
    assert (sdir / "room1_transcript.jsonl").read_text() == before


def test_clean_transcript_says_so(tmp_path, monkeypatch):
    sdir = _make_session(tmp_path, monkeypatch, ["no names here"])
    monkeypatch.setattr(redact.llm, "chat",
                        lambda *a, **k: 'Sure! {"names": []}')
    assert "No personal names" in redact.check_session(sdir.name).read_text()


def test_garbage_model_output_raises(tmp_path, monkeypatch):
    _make_session(tmp_path, monkeypatch, ["hi"])
    monkeypatch.setattr(redact.llm, "chat", lambda *a, **k: "I cannot help")
    with pytest.raises(RuntimeError):
        redact.check_session("20260101-100000")


def test_empty_session_raises(tmp_path, monkeypatch):
    monkeypatch.setattr(redact, "SESSIONS_DIR", str(tmp_path))
    (tmp_path / "empty").mkdir()
    with pytest.raises(RuntimeError):
        redact.check_session("empty")
