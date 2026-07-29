"""FastAPI server: serves the subtitle page and a per-room websocket feed."""
import asyncio
import json
from pathlib import Path

from fastapi import FastAPI, WebSocket, WebSocketDisconnect
from fastapi.responses import HTMLResponse

STATIC_DIR = Path(__file__).resolve().parent / "static"


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
        dead = []
        for ws in self._rooms.get(room, set()):
            try:
                await ws.send_text(json.dumps(message, ensure_ascii=False))
            except Exception:  # client gone; drop it, page reconnects
                dead.append(ws)
        for ws in dead:
            self.unregister(room, ws)

    def broadcast_from_thread(self, room: str, message: dict) -> None:
        assert self.loop is not None, "server not started"
        asyncio.run_coroutine_threadsafe(self.broadcast(room, message), self.loop)


hub = Hub()
app = FastAPI(title="forum-agent")


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


@app.get("/api/status")
async def api_status() -> dict:
    from forum_agent.session import manager
    return manager.status()


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
    return await anyio.to_thread.run_sync(
        lambda: manager.start(body.get("source", "replay"),
                              body.get("room", "room1"),
                              play=bool(body.get("play", True)),
                              device=(int(body["device"])
                                      if body.get("device") not in (None, "", "auto")
                                      else None)))


@app.post("/api/stop")
async def api_stop() -> dict:
    from forum_agent.session import manager
    import anyio
    return await anyio.to_thread.run_sync(manager.stop)


@app.get("/insights", response_class=HTMLResponse)
async def insights_page() -> str:
    return (STATIC_DIR / "insights.html").read_text()


@app.get("/api/insights")
async def api_insights(room: str = "room1", session: str = "") -> dict:
    if session:  # archived session: read-only snapshot
        import json
        from forum_agent.constants import SESSIONS_DIR
        from pathlib import Path
        p = Path(SESSIONS_DIR) / session / f"{room}_insights.json"
        if p.exists():
            return {**json.loads(p.read_text()), "archived": session}
        return {"items": {}, "convergence_line": {}, "archived": session}
    from forum_agent.insights import engine
    e = engine(room)
    return {**e.state, "error": e.error}


@app.get("/api/sessions")
async def api_sessions() -> list:
    from forum_agent.session import list_sessions
    return list_sessions()


@app.post("/api/sessions/delete")
async def api_sessions_delete(body: dict) -> dict:
    from forum_agent.session import delete_session
    return {"deleted": delete_session(body.get("id", ""))}


@app.post("/api/insights/run")
async def api_insights_run(body: dict) -> dict:
    """Manual 'Summarize now' from the operator console."""
    from forum_agent.insights import engine
    import anyio
    return await anyio.to_thread.run_sync(
        engine(body.get("room", "room1")).refresh)


@app.post("/api/insights/mode")
async def api_insights_mode(body: dict) -> dict:
    from forum_agent.insights import engine
    e = engine(body.get("room", "room1"))
    e.auto_approve = bool(body.get("auto_approve", True))
    return {"auto_approve": e.auto_approve}


@app.post("/api/insights/item")
async def api_insights_item(body: dict) -> dict:
    from forum_agent.insights import engine
    return engine(body.get("room", "room1")).set_item(
        body.get("id", ""), body.get("action", ""),
        body.get("zh", ""), body.get("en", ""))


@app.post("/api/minutes")
async def api_minutes(body: dict) -> dict:
    from forum_agent.insights import engine
    import anyio
    path = await anyio.to_thread.run_sync(
        engine(body.get("room", "room1")).generate_minutes)
    return {"path": path}


@app.get("/minutes", response_class=HTMLResponse)
async def minutes_page(room: str = "room1", session: str = "") -> str:
    from forum_agent.constants import MINUTES_MD, SESSIONS_DIR
    from pathlib import Path
    p = (Path(SESSIONS_DIR) / session / f"{room}_minutes.md") if session \
        else Path(MINUTES_MD.format(room=room))
    body = p.read_text() if p.exists() else "No minutes generated yet."
    return ("<!doctype html><meta charset=utf-8><title>Minutes</title>"
            "<body style='background:#0b0f14;color:#f2f5f7;font-family:"
            "-apple-system,PingFang SC,sans-serif;max-width:800px;"
            "margin:40px auto;line-height:1.6'><pre style='white-space:"
            f"pre-wrap;font:inherit'>{body}</pre>")


@app.get("/api/transcript")
async def api_transcript(room: str = "room1", session: str = ""):
    from forum_agent.constants import SESSIONS_DIR, TRANSCRIPT_JSONL
    from pathlib import Path
    from fastapi.responses import PlainTextResponse
    p = (Path(SESSIONS_DIR) / session / f"{room}_transcript.jsonl") if session \
        else Path(TRANSCRIPT_JSONL.format(room=room))
    return PlainTextResponse(p.read_text() if p.exists() else "")


@app.websocket("/ws/room/{room}")
async def room_ws(ws: WebSocket, room: str) -> None:
    await hub.register(room, ws)
    try:
        while True:
            await ws.receive_text()  # keepalive pings from the page
    except WebSocketDisconnect:
        hub.unregister(room, ws)


def main() -> None:
    """Persistent server: sessions are started from the /control page."""
    import uvicorn
    from forum_agent.constants import SERVER_HOST, SERVER_PORT
    print(f"Control: http://{SERVER_HOST}:{SERVER_PORT}/control")
    # Import string (not the app object): under `python -m forum_agent.server`
    # this file is module `__main__`, and passing its app would leave the
    # canonical forum_agent.server.hub -- the one the pipeline imports --
    # without an event loop.
    uvicorn.run("forum_agent.server:app", host=SERVER_HOST, port=SERVER_PORT,
                log_level="warning")


if __name__ == "__main__":
    main()
