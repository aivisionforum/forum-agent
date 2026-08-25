"""FastAPI server: pages, control API, and per-room websocket feed.

Security model: binds 127.0.0.1 only, but that does not stop CSRF/DNS
rebinding from a hostile page in the operator's browser — so every state-
changing POST requires a local Origin (or none, for curl), and all
session/room parameters are strictly validated against path traversal."""
import asyncio
import json
import re
from pathlib import Path

from fastapi import FastAPI, HTTPException, Request, WebSocket, \
    WebSocketDisconnect
from fastapi.responses import HTMLResponse, PlainTextResponse

STATIC_DIR = Path(__file__).resolve().parent / "static"
_SESSION_RE = re.compile(r"\d{8}-\d{6}")
_ROOM_RE = re.compile(r"[A-Za-z0-9_-]{1,32}")
_LOCAL_ORIGINS = ("http://127.0.0.1", "http://localhost")


def safe_session(session: str) -> str:
    if session and not _SESSION_RE.fullmatch(session):
        raise HTTPException(400, "invalid session id")
    return session


def safe_room(room: str) -> str:
    if not _ROOM_RE.fullmatch(room):
        raise HTTPException(400, "invalid room")
    return room


class Hub:
    """Fan-out of pipeline events to all subtitle-page websockets of a room."""

    def __init__(self) -> None:
        self._rooms: dict[str, set[WebSocket]] = {}
        self.loop: asyncio.AbstractEventLoop | None = None

    async def register(self, room: str, ws: WebSocket) -> None:
        await ws.accept()
        self._rooms.setdefault(room, set()).add(ws)

    def unregister(self, room: str, ws: WebSocket) -> None:
        self._rooms.get(room, set()).discard(ws)

    async def broadcast(self, room: str, message: dict) -> None:
        # iterate a copy: register/unregister can run while send() yields
        for ws in list(self._rooms.get(room, set())):
            try:
                await ws.send_text(json.dumps(message, ensure_ascii=False))
            except Exception:  # client gone; drop it, page reconnects
                self.unregister(room, ws)

    def broadcast_from_thread(self, room: str, message: dict) -> None:
        if self.loop is None:
            # standalone use (scripts, batch jobs): no pages to update
            return
        fut = asyncio.run_coroutine_threadsafe(
            self.broadcast(room, message), self.loop)
        fut.add_done_callback(
            lambda f: f.exception() and print(f"[hub] broadcast failed: "
                                              f"{f.exception()!r}"))


hub = Hub()
app = FastAPI(title="forum-agent")


@app.middleware("http")
async def _reject_cross_origin_posts(request: Request, call_next):
    if request.method == "POST":
        origin = request.headers.get("origin", "")
        if origin and not origin.startswith(_LOCAL_ORIGINS):
            return PlainTextResponse("cross-origin rejected", status_code=403)
    return await call_next(request)


@app.on_event("startup")
async def _capture_loop() -> None:
    hub.loop = asyncio.get_running_loop()


@app.get("/subtitles", response_class=HTMLResponse)
async def subtitles_page() -> str:
    return (STATIC_DIR / "subtitles.html").read_text()


@app.get("/", response_class=HTMLResponse)
@app.middleware("http")
async def no_html_cache(request: Request, call_next):
    """Wall displays are rarely hard-refreshed: stale cached pages kept
    running old JS after fixes. HTML is tiny — always revalidate."""
    resp = await call_next(request)
    if resp.headers.get("content-type", "").startswith("text/html"):
        resp.headers["Cache-Control"] = "no-store"
    return resp


@app.get("/control", response_class=HTMLResponse)
async def control_page() -> str:
    return (STATIC_DIR / "control.html").read_text()


@app.get("/insights", response_class=HTMLResponse)
async def insights_page() -> str:
    return (STATIC_DIR / "insights.html").read_text()


@app.get("/api/status")
async def api_status() -> dict:
    from forum_agent import llm
    from forum_agent.session import manager
    status = manager.status()
    # surface a dead LLM server on the console instead of failing silently
    status["llm"] = llm.healthy()
    status["llm_state"] = llm.state()  # "recovering" while watchdog relaunches
    from forum_agent import activity, preflight
    from forum_agent.constants import MINUTES_MD, REPORT_MD
    status["ram_warning"] = preflight.warning
    status["activity"] = activity.current()
    status["report_exists"] = Path(REPORT_MD).exists()
    status["minutes_exists"] = Path(
        MINUTES_MD.format(room=status["room"])).exists()
    return status


@app.get("/api/devices")
async def api_devices() -> list:
    import sounddevice as sd
    return [{"index": i, "name": d["name"]}
            for i, d in enumerate(sd.query_devices())
            if d["max_input_channels"] > 0]


@app.post("/api/start")
async def api_start(body: dict) -> dict:
    from forum_agent.session import manager
    import anyio
    room = safe_room(body.get("room", "room1"))
    return await anyio.to_thread.run_sync(
        lambda: manager.start(body.get("source", "replay"), room,
                              play=bool(body.get("play", True)),
                              record=bool(body.get("record", True)),
                              device=(int(body["device"])
                                      if body.get("device")
                                      not in (None, "", "auto") else None)))


@app.post("/api/stop")
async def api_stop() -> dict:
    from forum_agent.session import manager
    import anyio
    return await anyio.to_thread.run_sync(manager.stop)


@app.get("/api/insights")
async def api_insights(room: str = "room1", session: str = "") -> dict:
    from forum_agent.constants import INSIGHT_INTERVAL_SECONDS, SESSIONS_DIR
    safe_room(room), safe_session(session)
    if session:  # archived session: read-only snapshot
        p = Path(SESSIONS_DIR) / session / f"{room}_insights.json"
        state = json.loads(p.read_text()) if p.exists() else \
            {"items": {}, "convergence_line": {}}
        return {**state, "archived": session}
    from forum_agent.insights import engine
    from forum_agent import activity
    e = engine(room)
    return {**e.state, "error": e.error, "busy": activity.busy("insights"),
            "auto_started": e.auto_started, "now": __import__("time").time(),
            "interval": INSIGHT_INTERVAL_SECONDS}


@app.get("/api/sessions")
async def api_sessions() -> list:
    from forum_agent.session import list_sessions
    return list_sessions()


@app.post("/api/sessions/rename")
async def api_sessions_rename(body: dict) -> dict:
    from forum_agent.session import rename_session
    return {"renamed": rename_session(body.get("id", ""),
                                      body.get("title", ""))}


@app.post("/api/sessions/delete")
async def api_sessions_delete(body: dict) -> dict:
    from forum_agent.session import delete_session
    return {"deleted": delete_session(body.get("id", ""))}


def _selected_sessions(body: dict) -> list[str]:
    """Validated archive ids from the console. Live session wins: callers
    only use this when nothing is running."""
    from forum_agent.constants import SESSIONS_DIR
    ids = body.get("sessions") or []
    if not isinstance(ids, list) or not all(isinstance(x, str) for x in ids):
        raise HTTPException(400, "sessions must be a list of session ids")
    out = []
    for sid in ids:
        if "/" in sid or "\\" in sid or sid in (".", ".."):
            raise HTTPException(400, f"invalid session id: {sid!r}")
        if not Path(SESSIONS_DIR, sid).is_dir():
            raise HTTPException(404, f"unknown session: {sid}")
        out.append(sid)
    return out


@app.post("/api/insights/run")
async def api_insights_run(body: dict) -> dict:
    """Manual 'Summarize now'. Live session takes priority; when idle,
    runs per selected archived session."""
    from forum_agent.insights import engine
    from forum_agent.session import manager
    import anyio
    import functools
    room = safe_room(body.get("room", "room1"))
    e = engine(room)
    try:
        if not manager.status()["running"]:
            selected = _selected_sessions(body)
            if selected:
                for sid in selected:
                    await anyio.to_thread.run_sync(
                        functools.partial(e.refresh_for, sid))
                return {"processed": selected}
        return await anyio.to_thread.run_sync(e.refresh)
    except RuntimeError as exc:  # nothing to summarize anywhere
        raise HTTPException(409, str(exc))


@app.post("/api/insights/mode")
async def api_insights_mode(body: dict) -> dict:
    from forum_agent.insights import engine
    e = engine(safe_room(body.get("room", "room1")))
    e.auto_approve = bool(body.get("auto_approve", True))
    return {"auto_approve": e.auto_approve}


@app.post("/api/insights/item")
async def api_insights_item(body: dict) -> dict:
    from forum_agent.insights import engine
    return engine(safe_room(body.get("room", "room1"))).set_item(
        body.get("id", ""), body.get("action", ""),
        body.get("zh", ""), body.get("en", ""))


@app.post("/api/minutes")
async def api_minutes(body: dict) -> dict:
    """Live session takes priority; when idle, generates minutes for each
    selected archived session."""
    from forum_agent.insights import engine
    from forum_agent.session import manager
    import anyio
    import functools
    room = safe_room(body.get("room", "room1"))
    e = engine(room)
    try:
        if not manager.status()["running"]:
            selected = _selected_sessions(body)
            if selected:
                for sid in selected:
                    await anyio.to_thread.run_sync(
                        functools.partial(e.minutes_for, sid))
                return {"processed": selected}
        path = await anyio.to_thread.run_sync(e.generate_minutes)
        return {"path": path}
    except RuntimeError as exc:
        raise HTTPException(409, str(exc))


@app.post("/api/report")
async def api_report(body: dict) -> dict:
    from forum_agent.report import generate_report
    import anyio
    import functools
    selected = body.get("sessions") or None
    if selected is not None and not (
            isinstance(selected, list)
            and all(isinstance(x, str) for x in selected)):
        raise HTTPException(400, "sessions must be a list of session ids")
    try:
        path = await anyio.to_thread.run_sync(
            functools.partial(generate_report, selected))
    except RuntimeError as exc:  # nothing to synthesize, same as C6/C4
        raise HTTPException(409, str(exc))
    return {"path": path}


@app.get("/report", response_class=HTMLResponse)
async def report_page() -> str:
    from forum_agent import activity, pages
    return pages.report_page(busy=activity.busy("report"))


@app.get("/minutes", response_class=HTMLResponse)
async def minutes_page(room: str = "room1", session: str = "") -> str:
    from forum_agent import activity, pages
    return pages.render_markdown_page(
        "Minutes", pages.minutes_path(safe_room(room), safe_session(session)),
        "No minutes generated yet. They are generated automatically when a "
        "session stops, or on demand from the control console.",
        busy=activity.busy("minutes"))


@app.post("/api/ingest")
async def api_ingest(request: Request, name: str = "upload",
                     room: str = "room1") -> dict:
    """Upload a recording (raw bytes body, any ffmpeg-readable format) and
    batch-process it into an archived session (issue #8)."""
    import subprocess
    import tempfile
    import anyio
    from forum_agent.constants import SAMPLE_RATE
    from forum_agent.session import manager
    room = safe_room(room)
    if manager.status()["running"]:
        raise HTTPException(409, "stop the live session before uploading")
    data = await request.body()
    if not data:
        raise HTTPException(400, "empty upload")
    name = Path(name).name  # strip any path the browser sent
    with tempfile.TemporaryDirectory() as td:
        src = Path(td) / name
        src.write_bytes(data)
        wav = Path(td) / "converted.wav"
        conv = subprocess.run(
            ["ffmpeg", "-y", "-i", str(src), "-ac", "1",
             "-ar", str(SAMPLE_RATE), str(wav)],
            capture_output=True, text=True)
        if conv.returncode != 0:
            raise HTTPException(
                400, f"ffmpeg could not read the file: {conv.stderr[-400:]}")
        try:
            sid = await anyio.to_thread.run_sync(
                lambda: manager.ingest(str(wav), room, title=name))
        except RuntimeError as exc:
            raise HTTPException(409, str(exc))
    if sid is None:
        raise HTTPException(422, "no speech detected in the upload")
    return {"session": sid}


@app.get("/api/polish/config")
async def api_polish_config() -> dict:
    """Sanitized cloud-polish settings for the console form. Reports whether
    a key is set and where it came from — never the key itself."""
    from forum_agent import cloud
    return cloud.config_view()


@app.post("/api/polish/config")
async def api_polish_config_save(body: dict) -> dict:
    """Save console-entered cloud settings to data/cloud_config.json
    (gitignored, chmod 600). Empty string clears a field. The server binds
    to 127.0.0.1 only, so this form is reachable solely from this machine."""
    from forum_agent import cloud
    return cloud.save_config(body if isinstance(body, dict) else {})


@app.get("/api/polish/providers")
async def api_polish_providers() -> list:
    from forum_agent import cloud
    return cloud.providers()


@app.post("/api/polish")
async def api_polish(body: dict) -> dict:
    """Post-event cloud polish (issue #15): explicit opt-in per run; sends
    ONE local draft (a session's minutes, or the event report) to the chosen
    cloud provider and writes *_polished.md beside it."""
    import anyio
    import functools
    from forum_agent import cloud
    from forum_agent.constants import REPORT_MD, SESSIONS_DIR
    from forum_agent.session import manager
    if manager.status()["running"]:
        raise HTTPException(409, "post-event only: stop the session first")
    provider = str(body.get("provider", ""))
    model = str(body.get("model", ""))
    if not provider or not model:
        raise HTTPException(400, "provider and model are required")
    target = body.get("target", "minutes")
    if target == "report":
        src, dest = Path(REPORT_MD), Path(REPORT_MD).with_name(
            "report_polished.md")
    else:
        sids = _selected_sessions(body)
        if len(sids) != 1:
            raise HTTPException(400, "pick exactly one session to polish")
        d = Path(SESSIONS_DIR) / sids[0]
        src, dest = d / "room1_minutes.md", d / "room1_minutes_polished.md"
    try:
        await anyio.to_thread.run_sync(functools.partial(
            cloud.polish_file, src, dest, provider, model))
    except RuntimeError as exc:
        raise HTTPException(409, str(exc))
    except Exception as exc:  # provider/network errors, surfaced verbatim
        raise HTTPException(502, f"cloud polish failed: {exc}")
    return {"polished": str(dest), "target": target}


@app.get("/polished", response_class=HTMLResponse)
async def polished_page(session: str = "", target: str = "minutes") -> str:
    from forum_agent import activity, pages
    from forum_agent.constants import REPORT_MD, SESSIONS_DIR
    if target == "report":
        p = Path(REPORT_MD).with_name("report_polished.md")
    else:
        safe_session(session)
        p = Path(SESSIONS_DIR) / session / "room1_minutes_polished.md"
    return pages.render_markdown_page(
        "Polished (cloud)", p, "No polished version yet.",
        busy=activity.busy("cloud"))


@app.post("/api/speak")
async def api_speak(body: dict) -> dict:
    """Invoked voice summary (issue #14): read the current approved key
    points aloud. Moderator-triggered only — the agent never speaks
    uninvited."""
    import anyio
    from forum_agent import speak
    from forum_agent.insights import engine
    room = safe_room(body.get("room", "room1"))
    try:
        await anyio.to_thread.run_sync(
            lambda: speak.speak_summary(engine(room).state))
    except RuntimeError as exc:
        raise HTTPException(409, str(exc))
    return {"spoken": True}


@app.post("/api/redact")
async def api_redact(body: dict) -> dict:
    """Name check (issue #10): list personal names in archived transcripts
    for operator review. Advisory only — modifies no files."""
    import functools
    import anyio
    from forum_agent import redact
    from forum_agent.session import manager
    if manager.status()["running"]:
        raise HTTPException(409, "stop the session before the name check")
    ids = _selected_sessions(body)
    if not ids:
        raise HTTPException(400, "pick at least one session")
    for sid in ids:
        try:
            await anyio.to_thread.run_sync(
                functools.partial(redact.check_session, sid))
        except RuntimeError as exc:
            raise HTTPException(409, str(exc))
    return {"processed": ids}


@app.get("/redaction", response_class=HTMLResponse)
async def redaction_page(session: str) -> str:
    from forum_agent import pages, redact
    from forum_agent.constants import SESSIONS_DIR
    safe_session(session)
    return pages.render_markdown_page(
        f"Name check — {session}",
        Path(SESSIONS_DIR) / session / redact.REDACTION_MD,
        "No name check run yet — use the console.")


@app.get("/transcript", response_class=HTMLResponse)
async def transcript_page(room: str = "room1", session: str = "") -> str:
    from forum_agent import pages
    return pages.transcript_page(safe_room(room), safe_session(session))


@app.get("/api/transcript")
async def api_transcript(room: str = "room1", session: str = ""):
    from forum_agent import pages
    tpath, _ = pages.transcript_paths(safe_room(room), safe_session(session))
    return PlainTextResponse(tpath.read_text() if tpath.exists() else "")


@app.get("/api/audio")
async def api_audio(room: str = "room1", session: str = ""):
    from forum_agent.constants import RECORDING_WAV, SESSIONS_DIR
    from fastapi.responses import FileResponse
    safe_room(room), safe_session(session)
    p = (Path(SESSIONS_DIR) / session / f"{room}_recording.wav") if session \
        else Path(RECORDING_WAV.format(room=room))
    if not p.exists():
        return PlainTextResponse("no recording", status_code=404)
    return FileResponse(p, media_type="audio/wav", filename=p.name)


@app.websocket("/ws/room/{room}")
async def room_ws(ws: WebSocket, room: str) -> None:
    origin = ws.headers.get("origin", "")
    if origin and not origin.startswith(_LOCAL_ORIGINS):
        await ws.close(code=4403)  # hostile page must not read the live feed
        return
    await hub.register(room, ws)
    try:
        while True:
            await ws.receive_text()  # keepalive pings from the page
    except WebSocketDisconnect:
        hub.unregister(room, ws)


def main() -> None:
    """Persistent server: sessions are started from the /control page.
    Also launches and owns the mlx-lm model server (one-command startup)."""
    import atexit
    import uvicorn
    from forum_agent import llm, preflight
    preflight.check()  # refuse/warn on undersized machines before loading
    from forum_agent.constants import SERVER_HOST, SERVER_PORT
    proc = llm.launch_server()
    llm.start_watchdog(proc)   # relaunch if it dies mid-forum (issue #9)
    atexit.register(llm.shutdown)  # terminates whichever proc we own by then
    import threading
    threading.Thread(target=llm.prewarm, daemon=True).start()
    print(f"Control: http://{SERVER_HOST}:{SERVER_PORT}/control")
    # Import string (not the app object): under `python -m forum_agent.server`
    # this file is module `__main__`, and passing its app would leave the
    # canonical forum_agent.server.hub -- the one the pipeline imports --
    # without an event loop.
    uvicorn.run("forum_agent.server:app", host=SERVER_HOST, port=SERVER_PORT,
                log_level="warning")


if __name__ == "__main__":
    main()
