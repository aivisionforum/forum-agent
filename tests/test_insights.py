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


def test_empty_transcript_is_noop(eng, monkeypatch, tmp_path):
    (tmp_path / "room1_t.jsonl").write_text("")
    called = []
    monkeypatch.setattr(ins, "_llm", lambda p: called.append(1) or LLM_JSON)
    eng.refresh()
    assert not called  # no LLM call on empty transcript (R14: no vacuous run)


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
