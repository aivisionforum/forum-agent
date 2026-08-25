"""Fault injection against a running forum-agent.

    .venv/bin/python -m forum_agent.server &          # in another shell
    .venv/bin/python -m stress.faults

Every check answers one question: when this goes wrong at the venue, does the
operator SEE it, or does the session quietly degrade? Verdicts are

  ok      handled, and visible where an operator would look
  risk    handled but silent, or recovery needs an operator who knows the fix
  bug     wrong behaviour

Nothing here fills the disk or opens the microphone for real capture.
"""
import json
import subprocess
import time
import urllib.error
import urllib.request

BASE = "http://127.0.0.1:8710"
LOCAL = {"origin": "http://127.0.0.1:8710", "content-type": "application/json"}
RESULTS = []


def _req(method: str, path: str, body=None, headers=None, timeout=30):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(BASE + path, data=data, method=method,
                                 headers=headers or {})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            raw = r.read().decode()
            try:
                return r.status, json.loads(raw)
            except json.JSONDecodeError:
                return r.status, raw[:200]
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode()[:200]
    except Exception as e:
        return None, f"{type(e).__name__}: {e}"


def get(p, **kw):
    return _req("GET", p, **kw)


def post(p, body, **kw):
    return _req("POST", p, body, headers=LOCAL, **kw)


def record(name: str, verdict: str, detail: str) -> None:
    RESULTS.append({"check": name, "verdict": verdict, "detail": detail})
    print(f"{verdict.upper():<5} {name}: {detail}", flush=True)


def status():
    return get("/api/status")[1]


def wait_idle(timeout=180) -> bool:
    end = time.time() + timeout
    while time.time() < end:
        if not status().get("running"):
            return True
        time.sleep(2)
    return False


def check_double_start():
    a = post("/api/start", {"source": "replay", "room": "room1", "play": False})
    b = post("/api/start", {"source": "replay", "room": "room1", "play": False})
    time.sleep(20)
    s = status()
    ok = s.get("running") and s.get("mode") == "replay"
    record("double Start click", "ok" if ok else "bug",
           f"second start returned {b[0]}; status running={s.get('running')} "
           f"error={s.get('error')!r}")


def check_stop_mid_inference():
    t0 = time.time()
    r = post("/api/stop", {}, timeout=200)
    dt = time.time() - t0
    s = status()
    sessions = get("/api/sessions")[1]
    archived = isinstance(sessions, list) and len(sessions) > 0
    record("Stop mid-inference", "ok" if s.get("error") is None else "risk",
           f"stop took {dt:.0f}s, error={s.get('error')!r}, "
           f"archived={archived}")


def _trans_lines(room: str = "room1") -> int:
    from pathlib import Path
    p = Path(f"data/{room}_translations.jsonl")
    return len(p.read_text().splitlines()) if p.exists() else 0


def check_llm_watchdog():
    """Kill the model server mid-session and watch the watchdog (issue #9).

    "Recovered" is not the health flag flipping back — it is translations
    reaching the subtitle file again, so this measures that too.
    """
    post("/api/start", {"source": "replay", "room": "room1", "play": False})
    time.sleep(25)  # let a couple of segments translate normally
    before = _trans_lines()
    subprocess.run(["pkill", "-f", "mlx_lm server"], check=False)
    killed_at = time.time()

    saw_recovering = False
    detect = recover = None
    deadline = killed_at + 240
    while time.time() < deadline:
        s = status()
        if s.get("llm_state") == "recovering" and detect is None:
            detect = time.time() - killed_at
            saw_recovering = True
        if detect is not None and s.get("llm") is True:
            recover = time.time() - killed_at
            break
        time.sleep(2)

    s = status()
    if recover is None:
        record("watchdog relaunches the model server", "bug",
               f"no recovery within 240s; llm={s.get('llm')} "
               f"state={s.get('llm_state')!r}")
    else:
        record("watchdog relaunches the model server", "ok",
               f"detected in {detect:.0f}s, healthy again {recover:.0f}s "
               f"after the kill; session kept running={s.get('running')}")
    record("console can show the outage", "ok" if saw_recovering else "risk",
           f"llm_state reached 'recovering' = {saw_recovering} "
           "(control.html renders it while the watchdog works)")

    # the flag is cheap; the real question is whether subtitles resume
    if recover is not None:
        end = time.time() + 150
        while time.time() < end and _trans_lines() <= before:
            time.sleep(3)
        after = _trans_lines()
        record("translations resume after recovery",
               "ok" if after > before else "bug",
               f"translation lines {before} -> {after}")

    # a second kill proves the watchdog adopted the NEW process
    subprocess.run(["pkill", "-f", "mlx_lm server"], check=False)
    k2 = time.time()
    ok2 = False
    while time.time() < k2 + 240:
        if status().get("llm") is True:
            ok2 = True
            break
        time.sleep(2)
    record("watchdog survives a second failure", "ok" if ok2 else "bug",
           f"recovered again = {ok2} after {time.time() - k2:.0f}s")

    procs = subprocess.run(["pgrep", "-f", "mlx_lm server"],
                           capture_output=True, text=True).stdout.split()
    record("no orphaned model servers", "ok" if len(procs) <= 1 else "bug",
           f"{len(procs)} mlx_lm process(es) running after two kills")

    post("/api/stop", {}, timeout=300)
    wait_idle(300)


def check_bad_device():
    r = post("/api/start", {"source": "mic", "room": "room1", "device": 99})
    time.sleep(8)
    s = status()
    surfaced = bool(s.get("error")) or not s.get("running")
    record("nonexistent mic device", "ok" if surfaced else "bug",
           f"start returned {r[0]}, status error={s.get('error')!r}, "
           f"running={s.get('running')}")
    post("/api/stop", {}, timeout=200)
    wait_idle()


def check_validation():
    cases = [
        ("room traversal", "/api/start",
         {"source": "replay", "room": "../../etc", "play": False}, 400),
        ("session traversal (insights)", None, None, None),
        ("delete traversal", "/api/sessions/delete", {"id": "../../.."}, 200),
        ("rename empty title", "/api/sessions/rename",
         {"id": "20260101-000000", "title": "   "}, 200),
        # sibling endpoints map RuntimeError to 409; api_report does not, so
        # "generate a report for a selection with no material" is a 500
        ("report bogus session", "/api/report", {"sessions": ["../etc"]}, 409),
        ("insights item bogus", "/api/insights/item",
         {"room": "room1", "id": "nope", "action": "approve"}, None),
        ("malformed body", "/api/insights/mode", {"auto_approve": "yes"}, None),
    ]
    for name, path, body, want in cases:
        if path is None:
            code, resp = get("/api/insights?room=room1&session=../../etc")
            record("session traversal (insights)",
                   "ok" if code == 400 else "bug", f"HTTP {code}")
            continue
        code, resp = post(path, body, timeout=120)
        detail = f"HTTP {code} {str(resp)[:90]}"
        verdict = "ok" if (want is None or code == want) else "bug"
        if name == "delete traversal" and isinstance(resp, dict):
            verdict = "ok" if resp.get("deleted") is False else "bug"
        record(name, verdict, detail)


def check_websockets():
    """Many projector/console pages open at once, one dying mid-session."""
    try:
        from websockets.sync.client import connect
    except Exception as e:
        record("many websocket clients", "risk", f"cannot test: {e}")
        return
    conns = []
    try:
        for _ in range(10):
            conns.append(connect(f"ws://127.0.0.1:8710/ws/room/room1",
                                 additional_headers=LOCAL))
        conns[0].close()  # simulate a page crash
        post("/api/start", {"source": "replay", "room": "room1", "play": False})
        time.sleep(25)
        s = status()
        record("10 websocket clients, one dropped",
               "ok" if s.get("running") else "bug",
               f"session running={s.get('running')} error={s.get('error')!r}")
    finally:
        for c in conns[1:]:
            try:
                c.close()
            except Exception:
                pass
        post("/api/stop", {}, timeout=200)
        wait_idle()


def check_hostile_origin_ws():
    try:
        from websockets.sync.client import connect
        connect("ws://127.0.0.1:8710/ws/room/room1",
                additional_headers={"origin": "http://evil.example"})
        record("hostile-origin websocket", "bug", "connection accepted")
    except Exception as e:
        record("hostile-origin websocket", "ok",
               f"rejected: {type(e).__name__}")


def main() -> None:
    if status().get("mode") is None:
        print("server not reachable on 8710")
        raise SystemExit(1)
    wait_idle()
    check_validation()
    check_hostile_origin_ws()
    check_double_start()
    check_stop_mid_inference()
    check_bad_device()
    check_websockets()
    check_llm_watchdog()
    from pathlib import Path
    Path("data/stress/faults.json").write_text(
        json.dumps(RESULTS, ensure_ascii=False, indent=1))
    counts = {}
    for r in RESULTS:
        counts[r["verdict"]] = counts.get(r["verdict"], 0) + 1
    print("\nsummary:", counts, flush=True)


if __name__ == "__main__":
    main()
