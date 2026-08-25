"""Registry of in-flight background AI tasks, so every page can tell the
user what the system is working on instead of showing an empty screen.
Thread-safe; entries auto-expire via `end()` in finally blocks."""
import threading
import time
from contextlib import contextmanager

_lock = threading.Lock()
_tasks: dict[int, dict] = {}
_next_id = 0


@contextmanager
def task(label: str):
    """Register a background task for its duration:
    with activity.task("generating minutes"): ..."""
    global _next_id
    with _lock:
        _next_id += 1
        tid = _next_id
        _tasks[tid] = {"label": label, "started": time.time()}
    try:
        yield
    finally:
        with _lock:
            _tasks.pop(tid, None)


def current() -> list[dict]:
    """[{label, seconds}] for every task in flight, oldest first."""
    now = time.time()
    with _lock:
        return [{"label": t["label"], "seconds": int(now - t["started"])}
                for t in sorted(_tasks.values(), key=lambda t: t["started"])]


def busy(fragment: str) -> dict | None:
    """The oldest in-flight task whose label contains `fragment`, or None."""
    return next((t for t in current() if fragment in t["label"]), None)
