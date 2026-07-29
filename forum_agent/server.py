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
    import markdown
    body = p.read_text() if p.exists() else "No minutes generated yet."
    html = markdown.markdown(body, extensions=["tables"])
    return ("<!doctype html><meta charset=utf-8><title>Minutes</title>"
            "<style>body{background:#0b0f14;color:#f2f5f7;font-family:"
            "-apple-system,'PingFang SC',sans-serif;max-width:800px;"
            "margin:40px auto;line-height:1.7;padding:0 20px}"
            "h2{border-bottom:1px solid #1c2530;padding-bottom:6px;"
            "margin:28px 0 12px}h3{color:#6ea8fe;margin:18px 0 8px}"
            "li{margin:6px 0}hr{border:0;border-top:1px solid #1c2530;"
            "margin:32px 0}blockquote{color:#fcd34d;border-left:3px solid "
            "#78350f;padding-left:12px;margin-bottom:20px}</style>"
            f"<body>{html}")


@app.get("/transcript", response_class=HTMLResponse)
async def transcript_page(room: str = "room1", session: str = "") -> str:
    """Human-readable transcript with translations; raw JSONL stays at
    /api/transcript."""
    import html as h
    import json as j
    from forum_agent.constants import SESSIONS_DIR, TRANSCRIPT_JSONL
    from pathlib import Path
    base = Path(SESSIONS_DIR) / session if session else Path("data")
    tpath = base / f"{room}_transcript.jsonl" if session \
        else Path(TRANSCRIPT_JSONL.format(room=room))
    xpath = base / f"{room}_translations.jsonl"
    trans = {}
    if xpath.exists():
        for i, line in enumerate(xpath.read_text().splitlines(), 1):
            e = j.loads(line)
            trans[e.get("id", i)] = e["translation"]
    rows = []
    colors = ["#6ea8fe", "#3ddc84", "#f0997b", "#ed93b1", "#facc15", "#a78bfa"]
    if tpath.exists():
        for i, line in enumerate(tpath.read_text().splitlines(), 1):
            r = j.loads(line)
            mm, ss = divmod(int(r["t_start"]), 60)
            c = colors[(ord(r["speaker_id"][-1]) - 65) % len(colors)]
            tr = trans.get(i, "")
            rows.append(
                f"<div class=seg><span class=t>{mm:02d}:{ss:02d}</span>"
                f"<span class=sp style='color:{c}'>{h.escape(r['speaker_id'])}</span>"
                f"<span class=lg>{r['lang']}</span>"
                f"<div class=tx>{h.escape(r['text'])}"
                + (f"<div class=tr>{h.escape(tr)}</div>" if tr else "")
                + "</div></div>")
    title = f"Transcript — {session or 'live'}"
    return ("<!doctype html><meta charset=utf-8><title>" + title + "</title>"
            "<style>body{background:#0b0f14;color:#f2f5f7;font-family:"
            "-apple-system,'PingFang SC',sans-serif;max-width:860px;"
            "margin:36px auto;padding:0 20px;line-height:1.6}"
            "h1{font-size:19px;margin-bottom:18px;color:#7f8c99}"
            ".seg{display:flex;gap:12px;margin-bottom:14px;align-items:baseline}"
            ".t{color:#556;font-size:13px;min-width:44px}"
            ".sp{font-weight:600;min-width:88px;font-size:14px}"
            ".lg{color:#556;font-size:12px;min-width:40px}"
            ".tx{flex:1;font-size:16px}"
            ".tr{color:#a8b8a8;font-size:14px;margin-top:2px}</style>"
            f"<h1>{title} · <a style='color:#6ea8fe' "
            f"href='/api/transcript?room={room}&session={session}'>raw JSONL"
            "</a></h1>" + ("".join(rows) or "No transcript."))


@app.get("/api/transcript")
async def api_transcript(room: str = "room1", session: str = ""):
    from forum_agent.constants import SESSIONS_DIR, TRANSCRIPT_JSONL
    from pathlib import Path
    from fastapi.responses import PlainTextResponse
    p = (Path(SESSIONS_DIR) / session / f"{room}_transcript.jsonl") if session \
        else Path(TRANSCRIPT_JSONL.format(room=room))
    return PlainTextResponse(p.read_text() if p.exists() else "")


@app.get("/api/audio")
async def api_audio(room: str = "room1", session: str = ""):
    from forum_agent.constants import RECORDING_WAV, SESSIONS_DIR
    from pathlib import Path
    from fastapi.responses import FileResponse, PlainTextResponse
    p = (Path(SESSIONS_DIR) / session / f"{room}_recording.wav") if session \
        else Path(RECORDING_WAV.format(room=room))
    if not p.exists():
        return PlainTextResponse("no recording", status_code=404)
    return FileResponse(p, media_type="audio/wav", filename=p.name)


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
