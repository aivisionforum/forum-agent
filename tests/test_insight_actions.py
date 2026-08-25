"""Operator actions must give visible, durable results: hide suppresses on
refresh, unhide lifts that suppression again."""
from forum_agent.insights import InsightEngine


def _engine_with_item():
    e = InsightEngine("room1")
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
