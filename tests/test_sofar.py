"""Issue #17: 'session so far' groups approved points into topic phases
(convergence-line changes) and shows each point once, where it first
surfaced."""
import json

from forum_agent import sofar


def _snap(updated, conv_zh, points):
    return json.dumps({"updated": updated,
                       "convergence_line": {"zh": conv_zh, "en": ""},
                       "items": {"summary_points": [
                           {"zh": z, "en": e, "status": st}
                           for z, e, st in points]}},
                      ensure_ascii=False)


def test_phases_split_on_convergence_change(tmp_path):
    (tmp_path / "room1_insights_history.jsonl").write_text("\n".join([
        _snap(100, "签证规则", [("过境规则", "transit", "approved")]),
        _snap(200, "签证规则", [("过境规则", "transit", "approved"),
                               ("入境点", "entry", "approved")]),
        _snap(300, "Linux桌面", [("桌面生态", "desktop", "approved"),
                                ("草稿点", "draft pt", "draft")]),
    ]))
    phases = sofar.build("room1", base_dir=str(tmp_path))
    assert len(phases) == 2
    assert phases[0]["label"] == "签证规则"
    assert [i["zh"] for i in phases[0]["items"]["summary_points"]] \
        == ["过境规则", "入境点"]          # dedup: 过境规则 appears once
    assert phases[1]["label"] == "Linux桌面"
    assert [i["zh"] for i in phases[1]["items"]["summary_points"]] \
        == ["桌面生态"]                    # drafts never included


def test_empty_and_torn_lines_are_safe(tmp_path):
    (tmp_path / "room1_insights_history.jsonl").write_text(
        "not json\n" + _snap(100, "话题", [("要点", "pt", "approved")]))
    phases = sofar.build("room1", base_dir=str(tmp_path))
    assert len(phases) == 1 and phases[0]["items"]["summary_points"]
    assert sofar.build("room1", base_dir=str(tmp_path / "missing")) == []
