"""Issue #12: insight items must be anchored to a verbatim transcript quote;
ungrounded items never auto-approve. Issue #13: post-session generation uses
the big model."""
import json

from forum_agent import insights
from forum_agent.constants import INSIGHT_MODEL, REPORT_MODEL
from forum_agent.insights import _is_grounded, _norm


TRANSCRIPT = "[0-10] Speaker A (zh): 我们应该优先保障言论自由，而不是商业利益。"
TNORM = _norm(TRANSCRIPT)


def test_exact_quote_is_grounded():
    assert _is_grounded({"quote": "优先保障言论自由"}, TNORM)


def test_quote_survives_punctuation_and_spacing():
    assert _is_grounded({"quote": "言论 自由，而不是 商业利益"}, TNORM)


def test_invented_or_missing_quote_is_not_grounded():
    assert not _is_grounded({"quote": "人工智能改变世界"}, TNORM)
    assert not _is_grounded({"quote": ""}, TNORM)
    assert not _is_grounded({}, TNORM)


def test_archived_ungrounded_item_stays_draft(tmp_path, monkeypatch):
    e = insights.InsightEngine("room1")
    state = {"updated": 0, "items": {k: [] for k in insights.KINDS},
             "convergence_line": {"zh": "", "en": ""}, "hidden_zh": [],
             "approved_log": {k: [] for k in insights.KINDS}}
    parsed = {"summary_points": [
        {"zh": "真实要点", "en": "real", "quote": "优先保障言论自由"},
        {"zh": "编造要点", "en": "made up", "quote": "完全无关的话"}]}
    monkeypatch.setattr(e, "_broadcast", lambda *a, **k: None,
                        raising=False)
    out = e._store_archived(str(tmp_path), state, parsed,
                            transcript=TRANSCRIPT)
    items = out["items"]["summary_points"]
    by_zh = {i["zh"]: i for i in items}
    assert by_zh["真实要点"]["status"] == "approved"
    assert by_zh["编造要点"]["status"] == "draft"
    # only grounded/approved points feed the minutes
    assert [i["zh"] for i in out["approved_log"]["summary_points"]] \
        == ["真实要点"]


def test_big_flag_routes_to_report_model(monkeypatch):
    calls = []
    monkeypatch.setattr(insights.llm, "chat",
                        lambda model, *a, **k: calls.append(model) or "x")
    insights._llm("p")
    insights._llm("p", big=True)
    assert calls == [INSIGHT_MODEL, REPORT_MODEL]


def test_carried_over_point_expires_when_topic_moves(monkeypatch):
    """Structural freshness: a point the model re-emits survives only while
    its quote appears in the CURRENT window."""
    import forum_agent.insights as ins
    e = ins.InsightEngine("room1")
    e.state = {"updated": 0, "items": {k: [] for k in ins.KINDS},
               "convergence_line": {"zh": "", "en": ""}, "hidden_zh": [],
               "approved_log": {k: [] for k in ins.KINDS}}
    e.state["items"]["summary_points"] = [
        {"id": "old1", "zh": "旧话题要点", "en": "old", "status": "approved",
         "quote": "签证十天规则", "added": 1}]
    monkeypatch.setattr(ins, "read_transcript",
                        lambda *a, **k: "[0-9] Speaker A (zh): 我们讨论行业战略与发展现状")
    monkeypatch.setattr(ins, "_llm", lambda p, **k: json.dumps({
        "summary_points": [
            {"zh": "旧话题要点", "en": "old", "quote": "签证十天规则"},
            {"zh": "行业战略讨论", "en": "strategy", "quote": "行业战略与发展现状"}],
        "next_steps": [], "emerging_consensus": [], "tensions": [],
        "open_questions": [],
        "convergence_line": {"zh": "x", "en": "x"},
        "session_topic": {"zh": "y", "en": "y"}}, ensure_ascii=False))
    monkeypatch.setattr(ins.hub, "broadcast_from_thread", lambda *a, **k: None)
    monkeypatch.setattr(e, "_save_and_broadcast", lambda: None)
    import forum_agent.session as fs
    monkeypatch.setattr(fs.manager, "status",
                        lambda: {"running": True})
    state = e.refresh()
    zhs = [i["zh"] for i in state["items"]["summary_points"]]
    assert "行业战略讨论" in zhs          # new-topic point admitted
    assert "旧话题要点" not in zhs        # stale point expired with its topic
