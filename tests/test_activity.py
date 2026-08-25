"""Background-task registry: pages report what is being generated."""
from forum_agent import activity


def test_task_visible_only_while_running():
    assert activity.busy("minutes") is None
    with activity.task("generating minutes (~1-2 min)"):
        cur = activity.current()
        assert cur and cur[0]["label"].startswith("generating minutes")
        assert activity.busy("minutes")["seconds"] >= 0
        assert activity.busy("report") is None
    assert activity.current() == []


def test_task_cleared_even_on_error():
    try:
        with activity.task("drafting event report"):
            raise ValueError("llm failed")
    except ValueError:
        pass
    assert activity.busy("report") is None
