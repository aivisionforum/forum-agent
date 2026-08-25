"""Issue #15: cloud polish is env-gated, provenance-labelled, and never
modifies the local draft."""
import pytest

from forum_agent import cloud


def test_no_credentials_means_no_providers(monkeypatch):
    monkeypatch.delenv("OPENROUTER_API_KEY", raising=False)
    monkeypatch.setattr(cloud.requests, "get",
                        lambda *a, **k: (_ for _ in ()).throw(
                            cloud.requests.RequestException()))
    assert cloud.providers() == []


def test_openrouter_provider_appears_with_key(monkeypatch):
    monkeypatch.setenv("OPENROUTER_API_KEY", "test-key")
    monkeypatch.setattr(cloud.requests, "get",
                        lambda *a, **k: (_ for _ in ()).throw(
                            cloud.requests.RequestException()))
    provs = cloud.providers()
    assert provs and provs[0]["id"] == "openrouter"


def test_polish_writes_labelled_copy_and_keeps_draft(tmp_path, monkeypatch):
    src = tmp_path / "m.md"
    src.write_text("# draft 纪要")
    monkeypatch.setattr(cloud, "_chat", lambda p, m, t: "# polished 纪要")
    dest = cloud.polish_file(src, tmp_path / "m_polished.md",
                             "openrouter", "some/model")
    out = dest.read_text()
    assert "CLOUD model" in out and "openrouter" in out  # provenance banner
    assert "polished" in out
    assert src.read_text() == "# draft 纪要"  # the draft is never modified


def test_missing_draft_raises(tmp_path):
    with pytest.raises(RuntimeError):
        cloud.polish_file(tmp_path / "none.md", tmp_path / "o.md",
                          "openrouter", "m")
