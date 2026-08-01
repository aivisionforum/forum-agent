"""Unit tests for the insight engine: parsing, status persistence, minutes.
The LLM is mocked; integration with real Ollama runs in the Phase-2
acceptance script."""
import json

import pytest

import forum_agent.insights as ins


@pytest.fixture()
def eng(tmp_path, monkeypatch):
    monkeypatch.setattr(ins, "INSIGHTS_JSON", str(tmp_path / "{room}_i.json"))
    monkeypatch.setattr(ins, "MINUTES_MD", str(tmp_path / "{room}_m.md"))
    monkeypatch.setattr(ins, "TRANSCRIPT_JSONL",
                        str(tmp_path / "{room}_t.jsonl"))
    (tmp_path / "room1_t.jsonl").write_text(json.dumps(
        {"t_start": 0, "t_end": 5, "speaker_id": "Speaker A", "lang": "zh",
         "text": "我们需要 meaningful human control"}) + "\n")
    monkeypatch.setattr(ins.hub, "broadcast_from_thread",
                        lambda room, msg: None)
    return ins.InsightEngine("room1")


LLM_JSON = json.dumps({
    "summary_points": [{"zh": "要点一", "en": "Point one"}],
    "emerging_consensus": [{"zh": "共识一", "en": "Consensus one"}],
    "tensions": [], "open_questions": [],
    "convergence_line": {"zh": "收敛线", "en": "Converging line"}})


def test_refresh_parses_and_auto_approves(eng, monkeypatch):
    monkeypatch.setattr(ins, "_llm", lambda p: f"```json\n{LLM_JSON}\n```")
    state = eng.refresh()
    assert state["items"]["summary_points"][0]["zh"] == "要点一"
    assert state["items"]["summary_points"][0]["status"] == "approved"  # default
    assert state["convergence_line"]["en"] == "Converging line"


def test_gatekeeper_mode_marks_draft(eng, monkeypatch):
    eng.auto_approve = False
    monkeypatch.setattr(ins, "_llm", lambda p: LLM_JSON)
    state = eng.refresh()
    assert state["items"]["summary_points"][0]["status"] == "draft"


def test_panel_replaced_but_approvals_logged(eng, monkeypatch):
    eng.auto_approve = False
    monkeypatch.setattr(ins, "_llm", lambda p: LLM_JSON)
    state = eng.refresh()
    item_id = state["items"]["summary_points"][0]["id"]
    eng.set_item(item_id, "approved")
    new = json.dumps({"summary_points": [{"zh": "新要点", "en": "New"}],
                      "emerging_consensus": [], "tensions": [],
                      "open_questions": [],
                      "convergence_line": {"zh": "x", "en": "y"}})
    monkeypatch.setattr(ins, "_llm", lambda p: new)
    state = eng.refresh()
    zhs = [i["zh"] for i in state["items"]["summary_points"]]
    assert zhs == ["新要点"]  # panel is a live snapshot: old item retired
    assert {"zh": "要点一", "en": "Point one"} in \
        state["approved_log"]["summary_points"]  # ...but stays in the log


def test_hidden_items_stay_suppressed(eng, monkeypatch):
    monkeypatch.setattr(ins, "_llm", lambda p: LLM_JSON)
    state = eng.refresh()
    item_id = state["items"]["summary_points"][0]["id"]
    eng.set_item(item_id, "hidden")
    state = eng.refresh()  # LLM re-suggests the same item
    assert all(i["zh"] != "要点一"
               for i in state["items"]["summary_points"])
    assert all(e["zh"] != "要点一"
               for e in state["approved_log"]["summary_points"])


def test_empty_transcript_no_archive_raises(eng, monkeypatch, tmp_path):
    import forum_agent.session as fs
    monkeypatch.setattr(fs, "SESSIONS_DIR", str(tmp_path / "none"))
    (tmp_path / "room1_t.jsonl").write_text("")
    called = []
    monkeypatch.setattr(ins, "_llm", lambda p: called.append(1) or LLM_JSON)
    import pytest
    with pytest.raises(RuntimeError):
        eng.refresh()  # R14: nothing to summarize -> error, not vacuous run
    assert not called


def test_minutes_written_with_draft_banner(eng, monkeypatch):
    monkeypatch.setattr(ins, "_llm", lambda p: "## 会议纪要（草稿）\n- 要点")
    path = eng.generate_minutes()
    content = open(path).read()
    assert content.startswith("> DRAFT")
    assert "会议纪要" in content


def test_archive_and_list(tmp_path, monkeypatch):
    import forum_agent.session as fs
    monkeypatch.setattr(fs, "TRANSCRIPT_JSONL", str(tmp_path / "{room}_transcript.jsonl"))
    monkeypatch.setattr(fs, "INSIGHTS_JSON", str(tmp_path / "{room}_insights.json"))
    monkeypatch.setattr(fs, "MINUTES_MD", str(tmp_path / "{room}_minutes.md"))
    monkeypatch.setattr(fs, "SESSIONS_DIR", str(tmp_path / "sessions"))
    assert fs.archive_live("room1") is None  # nothing to archive
    (tmp_path / "room1_transcript.jsonl").write_text('{"t_end": 1}\n')
    (tmp_path / "room1_insights.json").write_text('{"items": {}}')
    sid = fs.archive_live("room1")
    assert sid is not None
    assert not (tmp_path / "room1_transcript.jsonl").exists()  # moved
    sessions = fs.list_sessions()
    assert sessions[0]["id"] == sid and sessions[0]["segments"] == 1
    assert "room1_insights.json" in sessions[0]["files"]


def test_delete_session_validates_id(tmp_path, monkeypatch):
    import forum_agent.session as fs
    monkeypatch.setattr(fs, "SESSIONS_DIR", str(tmp_path / "sessions"))
    d = tmp_path / "sessions" / "20260728-120000"
    d.mkdir(parents=True)
    assert not fs.delete_session("../../etc")   # traversal rejected
    assert not fs.delete_session("nonexistent")
    assert fs.delete_session("20260728-120000") and not d.exists()


def test_rename_and_default_title(tmp_path, monkeypatch):
    import json
    import forum_agent.session as fs
    monkeypatch.setattr(fs, "SESSIONS_DIR", str(tmp_path / "sessions"))
    monkeypatch.setattr(fs, "TRANSCRIPT_JSONL", str(tmp_path / "{room}_transcript.jsonl"))
    monkeypatch.setattr(fs, "INSIGHTS_JSON", str(tmp_path / "{room}_insights.json"))
    monkeypatch.setattr(fs, "MINUTES_MD", str(tmp_path / "{room}_minutes.md"))
    (tmp_path / "room1_transcript.jsonl").write_text(
        json.dumps({"t_end": 1, "text": "开场讨论 human agency"}) + "\n")
    sid = fs.archive_live("room1")
    assert fs.list_sessions()[0]["title"].startswith("开场讨论")
    assert fs.rename_session(sid, "  组会测试 Group Meeting  ")
    assert fs.list_sessions()[0]["title"] == "组会测试 Group Meeting"
    assert not fs.rename_session("../../x", "bad")


def test_report_gathers_sessions(tmp_path, monkeypatch):
    import json
    import forum_agent.report as rep
    monkeypatch.setattr(rep, "SESSIONS_DIR", str(tmp_path / "sessions"))
    monkeypatch.setattr(rep, "REPORT_MD", str(tmp_path / "report.md"))
    d = tmp_path / "sessions" / "20260729-100000"
    d.mkdir(parents=True)
    (d / "meta.json").write_text(json.dumps({"title": "主题一"}))
    (d / "room1_insights.json").write_text(json.dumps(
        {"approved_log": {"summary_points": [{"zh": "要点", "en": "Point"}]}}))
    captured = {}
    def fake_llm(prompt):
        captured["prompt"] = prompt
        return "# 报告\n## 摘要\n内容"
    monkeypatch.setattr(rep, "_llm", fake_llm)
    path = rep.generate_report()
    assert "主题一" in captured["prompt"] and "要点" in captured["prompt"]
    assert open(path).read().startswith("> DRAFT")
    import pathlib
    assert len(list(tmp_path.glob("report_*.md"))) == 1  # timestamped copy


def test_report_fails_loudly_when_empty(tmp_path, monkeypatch):
    import pytest
    import forum_agent.report as rep
    monkeypatch.setattr(rep, "SESSIONS_DIR", str(tmp_path / "none"))
    with pytest.raises(RuntimeError):
        rep.generate_report()  # R14: empty input is an error, not a report


def test_server_param_validation():
    import pytest
    from fastapi import HTTPException
    from forum_agent.server import safe_room, safe_session
    assert safe_session("20260728-191122") == "20260728-191122"
    assert safe_session("") == ""
    for bad in ["../../etc", "/etc/passwd", "20260728-191122/../x"]:
        with pytest.raises(HTTPException):
            safe_session(bad)
    assert safe_room("room1") == "room1"
    for bad in ["../room1", "room/1", "a" * 40, ""]:
        with pytest.raises(HTTPException):
            safe_room(bad)


def test_pages_escape_model_text(tmp_path, monkeypatch):
    import json
    from forum_agent import pages
    monkeypatch.setattr(pages, "TRANSCRIPT_JSONL", str(tmp_path / "{room}_t.jsonl"))
    monkeypatch.setattr(pages, "TRANSLATIONS_JSONL", str(tmp_path / "{room}_x.jsonl"))
    (tmp_path / "room1_t.jsonl").write_text(json.dumps(
        {"t_start": 0, "t_end": 1, "speaker_id": "Speaker A", "lang": "en",
         "text": "<img src=x onerror=alert(1)>"}) + "\n")
    html_out = pages.transcript_page("room1", "")
    assert "<img" not in html_out and "&lt;img" in html_out

    md = tmp_path / "m.md"
    md.write_text("# ok\n<script>alert(1)</script>")
    out = pages.render_markdown_page("T", md, "none")
    assert "<script>alert" not in out


def test_stale_refresh_redirects_to_archive(eng, monkeypatch, tmp_path):
    import json
    import forum_agent.session as fs
    monkeypatch.setattr(fs, "SESSIONS_DIR", str(tmp_path / "sessions"))
    monkeypatch.setattr(ins, "SESSIONS_DIR", str(tmp_path / "sessions"),
                        raising=False)
    d = tmp_path / "sessions" / "20260731-120000"
    d.mkdir(parents=True)
    (d / "room1_transcript.jsonl").write_text('{"t_end": 1}\n')

    def llm_and_stop(prompt):
        eng.stop_auto()  # session stops while the LLM call is in flight
        return LLM_JSON
    monkeypatch.setattr(ins, "_llm", llm_and_stop)
    state = eng.refresh()
    assert state.get("archived") == "20260731-120000"  # redirected, not lost
    saved = json.loads((d / "room1_insights.json").read_text())
    assert saved["items"]["summary_points"][0]["zh"] == "要点一"
    assert eng.state["updated"] == 0  # live state untouched
