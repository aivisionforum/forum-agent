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


def test_refresh_parses_and_marks_draft(eng, monkeypatch):
    monkeypatch.setattr(ins, "_llm", lambda p: f"```json\n{LLM_JSON}\n```")
    state = eng.refresh()
    assert state["items"]["summary_points"][0]["zh"] == "要点一"
    assert state["items"]["summary_points"][0]["status"] == "draft"
    assert state["convergence_line"]["en"] == "Converging line"


def test_approval_survives_refresh(eng, monkeypatch):
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
    by_zh = {i["zh"]: i for i in state["items"]["summary_points"]}
    assert by_zh["要点一"]["status"] == "approved"  # survived the refresh
    assert by_zh["新要点"]["status"] == "draft"


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
