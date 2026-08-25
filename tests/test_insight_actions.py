"""Operator actions must give visible, durable results: hide suppresses on
refresh, unhide lifts that suppression again."""
from forum_agent.insights import KINDS, InsightEngine


def _engine_with_item():
    e = InsightEngine("room1")
    # the constructor loads any persisted live state; start from a clean one
    e.state = {"updated": 0, "items": {k: [] for k in KINDS},
               "convergence_line": {"zh": "", "en": ""}, "hidden_zh": [],
               "approved_log": {k: [] for k in KINDS}}
    e.state["items"]["summary_points"] = [
        {"id": "i1", "zh": "要点", "en": "point", "status": "approved",
         "added": 0}]
    e._save_and_broadcast = lambda: None
    return e


def test_hide_then_unhide_lifts_suppression():
    e = _engine_with_item()
    e.set_item("i1", "hidden")
    assert e.state["items"]["summary_points"][0]["status"] == "hidden"
    assert "要点" in e.state["hidden_zh"]
    e.set_item("i1", "draft")   # the operator's "unhide"
    assert e.state["items"]["summary_points"][0]["status"] == "draft"
    assert "要点" not in e.state["hidden_zh"]


def test_approve_also_lifts_suppression():
    e = _engine_with_item()
    e.set_item("i1", "hidden")
    e.set_item("i1", "approved")
    assert "要点" not in e.state["hidden_zh"]


def test_unapprove_removes_from_minutes_log():
    e = _engine_with_item()
    e._log_approved("summary_points", e.state["items"]["summary_points"][0])
    e.set_item("i1", "draft")   # human gate: unapprove an auto-approved point
    assert e.state["approved_log"]["summary_points"] == []
    e.set_item("i1", "approved")  # and re-approving restores it
    assert e.state["approved_log"]["summary_points"][0]["zh"] == "要点"
