"""Startup resource check: refuse below minimum RAM unless forced; warn
between minimum and recommended."""
import pytest

from forum_agent import preflight


def _with_ram(monkeypatch, gb, force=False):
    monkeypatch.setattr(preflight, "total_ram_gb", lambda: gb)
    monkeypatch.setattr(preflight, "warning", None)
    if force:
        monkeypatch.setenv("FORUM_AGENT_FORCE", "1")
    else:
        monkeypatch.delenv("FORUM_AGENT_FORCE", raising=False)


def test_big_machine_is_clean(monkeypatch):
    _with_ram(monkeypatch, 128)
    preflight.check()
    assert preflight.warning is None


def test_below_minimum_refuses(monkeypatch):
    _with_ram(monkeypatch, 8)
    with pytest.raises(SystemExit):
        preflight.check()


def test_below_minimum_forced_starts_with_warning(monkeypatch):
    _with_ram(monkeypatch, 8, force=True)
    preflight.check()
    assert "forced start" in preflight.warning


def test_midsize_machine_warns_about_report_model(monkeypatch):
    _with_ram(monkeypatch, 32)
    preflight.check()
    assert "report model" in preflight.warning
