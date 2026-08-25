"""Issue #14: invoked voice summary speaks approved key points bilingually
and refuses when there is nothing approved to say."""
import pytest

from forum_agent import speak


def _state(points, line_zh=""):
    return {"items": {"summary_points": points},
            "convergence_line": {"zh": line_zh, "en": ""}}


def test_script_is_bilingual_and_approved_only():
    chunks = speak.build_script(_state([
        {"zh": "要点一", "en": "point one", "status": "approved"},
        {"zh": "草稿要点", "en": "draft point", "status": "draft"}],
        line_zh="整体方向"))
    text = " ".join(t for _, t in chunks)
    assert "要点一" in text and "point one" in text
    assert "草稿要点" not in text          # drafts are never spoken
    assert "整体方向" in text
    voices = {v for v, _ in chunks}
    assert voices == {speak.VOICE_ZH, speak.VOICE_EN}


def test_caps_at_max_points():
    pts = [{"zh": f"要点{i}", "en": f"p{i}", "status": "approved"}
           for i in range(8)]
    text = " ".join(t for _, t in speak.build_script(_state(pts)))
    assert f"要点{speak.MAX_POINTS - 1}" in text
    assert f"要点{speak.MAX_POINTS}" not in text


def test_nothing_approved_raises():
    with pytest.raises(RuntimeError):
        speak.build_script(_state([{"zh": "x", "en": "y", "status": "draft"}]))
