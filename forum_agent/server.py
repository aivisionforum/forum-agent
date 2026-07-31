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
    e = engine(room)
    return {**e.state, "error": e.error,
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


@app.post("/api/insights/run")
async def api_insights_run(body: dict) -> dict:
    """Manual 'Summarize now' from the operator console."""
    from forum_agent.insights import engine
    import anyio
    room = safe_room(body.get("room", "room1"))
    try:
        return await anyio.to_thread.run_sync(engine(room).refresh)
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
    from forum_agent.insights import engine
    import anyio
    room = safe_room(body.get("room", "room1"))
    path = await anyio.to_thread.run_sync(engine(room).generate_minutes)
    return {"path": path}


@app.post("/api/report")
async def api_report(body: dict) -> dict:
    from forum_agent.report import generate_report
    import anyio
    path = await anyio.to_thread.run_sync(generate_report)
    return {"path": path}


@app.get("/report", response_class=HTMLResponse)
async def report_page() -> str:
    from forum_agent import pages
    return pages.report_page()


@app.get("/minutes", response_class=HTMLResponse)
async def minutes_page(room: str = "room1", session: str = "") -> str:
    from forum_agent import pages
    return pages.render_markdown_page(
        "Minutes", pages.minutes_path(safe_room(room), safe_session(session)),
        "No minutes generated yet.")


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
    from forum_agent import llm
    from forum_agent.constants import SERVER_HOST, SERVER_PORT
    proc = llm.launch_server()
    if proc is not None:
        atexit.register(proc.terminate)
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
