"""Issue #9: the watchdog must relaunch a dead model server and surface a
"recovering" state while it does."""
from forum_agent import llm


def setup_function(_):
    llm._watchdog["state"] = "ok"
    llm._watchdog["proc"] = None


def test_healthy_resets_failures_and_state():
    llm._watchdog["state"] = "recovering"
    fails = llm._watchdog_step(2, check=lambda: True,
                               relaunch=lambda: (_ for _ in ()).throw(
                                   AssertionError("must not relaunch")))
    assert fails == 0
    assert llm.state() == "ok"


def test_relaunches_only_after_threshold():
    calls = []
    relaunch = lambda: calls.append(1) or True
    fails = llm._watchdog_step(0, check=lambda: False, relaunch=relaunch)
    assert fails == 1 and not calls        # below threshold: no relaunch
    fails = llm._watchdog_step(1, check=lambda: False, relaunch=relaunch)
    assert fails == 2 and not calls
    fails = llm._watchdog_step(2, check=lambda: False, relaunch=relaunch)
    assert calls and fails == 0            # third failure triggers relaunch
    assert llm.state() == "ok"


def test_failed_relaunch_stays_recovering_and_retries():
    fails = llm._watchdog_step(2, check=lambda: False, relaunch=lambda: False)
    assert fails == 3                      # keeps counting -> retries next tick
    assert llm.state() == "recovering"
    fails = llm._watchdog_step(fails, check=lambda: False,
                               relaunch=lambda: True)
    assert fails == 0 and llm.state() == "ok"
