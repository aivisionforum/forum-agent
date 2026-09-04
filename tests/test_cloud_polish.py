"""Issue #15: cloud polish is env-gated, provenance-labelled, and never
modifies the local draft."""
import pytest

from forum_agent import cloud


def test_no_credentials_means_no_providers(monkeypatch):
    monkeypatch.delenv("OPENROUTER_API_KEY", raising=False)
    monkeypatch.setattr(cloud.requests, "get",
                        lambda *a, **k: (_ for _ in ()).throw(
                            cloud.requests.RequestException()))
    monkeypatch.setattr(cloud, "_cli_path", lambda name: None)
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


def test_console_config_round_trip(tmp_path, monkeypatch):
    monkeypatch.setattr(cloud, "CLOUD_CONFIG_JSON",
                        str(tmp_path / "cloud_config.json"))
    monkeypatch.delenv("OPENROUTER_API_KEY", raising=False)
    view = cloud.save_config({"openrouter_api_key": "sk-test",
                              "openrouter_model": "meta/llama-4"})
    assert view["openrouter_configured"] is True
    assert view["openrouter_key_source"] == "file"
    assert "sk-test" not in str(view)          # key never leaves the server
    assert cloud._openrouter_key() == "sk-test"
    assert cloud._openrouter_model() == "meta/llama-4"
    # env still wins over the file
    monkeypatch.setenv("OPENROUTER_API_KEY", "sk-env")
    assert cloud._openrouter_key() == "sk-env"
    assert cloud.config_view()["openrouter_key_source"] == "env"
    # clearing with an empty string removes the stored key
    monkeypatch.delenv("OPENROUTER_API_KEY", raising=False)
    assert cloud.save_config({"openrouter_api_key": ""})[
        "openrouter_configured"] is False


def test_config_file_is_owner_only(tmp_path, monkeypatch):
    import os
    monkeypatch.setattr(cloud, "CLOUD_CONFIG_JSON",
                        str(tmp_path / "cloud_config.json"))
    cloud.save_config({"openrouter_api_key": "sk-x"})
    mode = os.stat(tmp_path / "cloud_config.json").st_mode & 0o777
    assert mode == 0o600


def test_cli_providers_detected_and_invoked(monkeypatch):
    monkeypatch.setattr(cloud, "_cli_path",
                        lambda name: f"/fake/bin/{name}")
    ids = [p["id"] for p in cloud.providers()]
    assert "claude" in ids and "codex" in ids
    calls = {}

    class R:
        returncode = 0
        stdout = "polished text"
        stderr = ""

    def fake_run(cmd, **kw):
        calls["cmd"] = cmd
        calls["input"] = kw.get("input")
        return R()

    monkeypatch.setattr(cloud.subprocess, "run", fake_run)
    out = cloud._chat("claude", "sonnet", "hello prompt")
    assert out == "polished text"
    assert calls["cmd"][:2] == ["/fake/bin/claude", "-p"]
    assert "--model" in calls["cmd"] and "sonnet" in calls["cmd"]
    assert calls["input"] == "hello prompt"
    out = cloud._chat("codex", "default", "hello prompt")
    assert calls["cmd"][0] == "/fake/bin/codex" and calls["cmd"][1] == "exec"


def test_cli_missing_raises(monkeypatch):
    monkeypatch.setattr(cloud, "_cli_path", lambda name: None)
    import pytest as _pytest
    with _pytest.raises(RuntimeError):
        cloud._chat_cli("claude", "default", "x")


def test_polish_archives_each_run_with_meta(tmp_path, monkeypatch):
    src = tmp_path / "m.md"
    src.write_text("# draft")
    monkeypatch.setattr(cloud, "_chat", lambda p, m, t: "# polished v")
    dest = tmp_path / "room1_minutes_polished.md"
    cloud.polish_file(src, dest, "claude", "opus", meta={"target": "minutes"})
    assert dest.exists()                       # latest, stable link target
    arch = list(tmp_path.glob("room1_minutes_polished_*_claude-opus.md"))
    assert len(arch) == 1                      # per-run archive with model
    import json as _json
    meta = _json.loads(arch[0].with_suffix(".json").read_text())
    assert meta["provider"] == "claude" and meta["target"] == "minutes"


def test_transcript_rides_along_when_provided(tmp_path, monkeypatch):
    src = tmp_path / "m.md"
    src.write_text("# draft")
    seen = {}
    monkeypatch.setattr(cloud, "_chat",
                        lambda p, m, t: seen.setdefault("prompt", t) or "out")
    cloud.polish_file(src, tmp_path / "o.md", "claude", "default",
                      transcript="[0-5] Speaker A (zh): 逐字内容")
    assert "逐字内容" in seen["prompt"]         # transcript included
    assert "Chatham House" in seen["prompt"]   # anonymity instruction rides
    seen.clear()
    cloud.polish_file(src, tmp_path / "o2.md", "claude", "default")
    assert "Transcript:" not in seen["prompt"]  # off by default
