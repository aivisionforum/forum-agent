"""Report session selection (operator picks which archives aggregate)."""
import json
from pathlib import Path

import pytest

from forum_agent import report


def _mk_session(root: Path, name: str, text: str) -> None:
    d = root / name
    d.mkdir(parents=True)
    (d / "room1_minutes.md").write_text(text)
    (d / "meta.json").write_text(json.dumps({"title": name}))


def test_selection_filters_sessions(tmp_path, monkeypatch):
    root = tmp_path / "sessions"
    _mk_session(root, "20260101-000000", "topic alpha")
    _mk_session(root, "20260102-000000", "topic beta")
    monkeypatch.setattr(report, "SESSIONS_DIR", str(root))
    monkeypatch.setattr(report, "REPORT_MD", str(tmp_path / "report.md"))
    seen = {}

    def fake_llm(prompt):
        seen["prompt"] = prompt
        return "# ok"
    monkeypatch.setattr(report, "_llm", fake_llm)

    report.generate_report(["20260102-000000"])
    assert "topic beta" in seen["prompt"]
    assert "topic alpha" not in seen["prompt"]
    header = (tmp_path / "report.md").read_text()
    assert "20260102-000000" in header
    assert "20260101-000000" not in header.splitlines()[1]


def test_empty_selection_matches_nothing(tmp_path, monkeypatch):
    root = tmp_path / "sessions"
    _mk_session(root, "20260101-000000", "topic alpha")
    monkeypatch.setattr(report, "SESSIONS_DIR", str(root))
    monkeypatch.setattr(report, "REPORT_MD", str(tmp_path / "report.md"))
    with pytest.raises(RuntimeError):
        report.generate_report(["20269999-000000"])
