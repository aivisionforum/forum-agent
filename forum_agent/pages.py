"""Server-rendered HTML pages (minutes, report, transcript). All dynamic
text is model- or speech-generated and therefore untrusted: everything is
HTML-escaped before rendering (XSS in the operator console would grant
session-delete authority)."""
import html
import json
from pathlib import Path

from forum_agent.constants import (MINUTES_MD, REPORT_MD, SESSIONS_DIR,
                                   TRANSCRIPT_JSONL, TRANSLATIONS_JSONL)

_BASE_CSS = ("body{background:#0b0f14;color:#f2f5f7;font-family:"
             "-apple-system,'PingFang SC',sans-serif;max-width:840px;"
             "margin:40px auto;line-height:1.7;padding:0 20px}"
             "h1{font-size:22px;margin:24px 0 12px}"
             "h2{border-bottom:1px solid #1c2530;padding-bottom:6px;"
             "margin:28px 0 12px}h3{color:#6ea8fe;margin:18px 0 8px}"
             "li{margin:6px 0}hr{border:0;border-top:1px solid #1c2530;"
             "margin:34px 0}blockquote{color:#fcd34d;border-left:3px solid "
             "#78350f;padding-left:12px;margin-bottom:20px}")


def _shell(title: str, body_html: str, extra_css: str = "") -> str:
    return (f"<!doctype html><meta charset=utf-8><title>{html.escape(title)}"
            f"</title><style>{_BASE_CSS}{extra_css}</style><body>{body_html}")


def _busy_banner(busy: dict | None) -> str:
    """Progress notice + 5s auto-reload while a background AI task runs, so
    an empty or stale page never looks broken."""
    if not busy:
        return ""
    return ("<meta http-equiv=refresh content=5>"
            "<blockquote>⏳ <b>WORKING / 生成中</b> — "
            f"{html.escape(busy['label'])}, running for {busy['seconds']}s. "
            "This page reloads automatically. / 页面将自动刷新。</blockquote>")


def render_markdown_page(title: str, path: Path, empty_msg: str,
                         busy: dict | None = None) -> str:
    """Markdown file -> page. Source text is escaped first so raw HTML in
    model output renders inert; markdown structure still formats."""
    import markdown
    text = path.read_text() if path.exists() else empty_msg
    md = markdown.markdown(html.escape(text), extensions=["tables"])
    return _shell(title, _busy_banner(busy) + md)


def minutes_path(room: str, session: str) -> Path:
    return (Path(SESSIONS_DIR) / session / f"{room}_minutes.md") if session \
        else Path(MINUTES_MD.format(room=room))


def _session_has_material(d: Path, room: str = "room1") -> bool:
    return (d / f"{room}_insights.json").exists() or \
        (d / f"{room}_minutes.md").exists()


def report_page(busy: dict | None = None) -> str:
    """Whole-event synthesis. Warns when sessions were archived after the
    report was generated, so a stale draft is never mistaken for current."""
    import markdown
    rp = Path(REPORT_MD)
    text = rp.read_text() if rp.exists() else "No report generated yet."
    banner = ""
    root = Path(SESSIONS_DIR)
    if rp.exists() and root.exists():
        newer = sorted(d.name for d in root.iterdir() if d.is_dir()
                       and _session_has_material(d)
                       and d.stat().st_mtime > rp.stat().st_mtime)
        live_tr = Path(TRANSCRIPT_JSONL.format(room="room1"))
        if live_tr.exists() and live_tr.stat().st_mtime > rp.stat().st_mtime:
            newer.append("current live session / 当前进行中的会议")
        if newer:
            names = ", ".join(newer)
            banner = ("<blockquote><b>STALE / 报告未包含最新会议</b> — "
                      f"generated before session(s): {html.escape(names)}. "
                      "Click “Generate event report” in the control console "
                      "to refresh. / 请在控制台点击“Generate event report”"
                      "重新生成。</blockquote>")
    md = markdown.markdown(html.escape(text), extensions=["tables"])
    return _shell("Report", _busy_banner(busy) + banner + md)


def sofar_page(room: str) -> str:
    """Operator-only 'session so far': approved points grouped into topic
    phases (issue #17). Auto-reloads; never shown on the projector."""
    import time as _time
    from forum_agent.sofar import KIND_TITLES, build, last_updated
    phases = build(room)
    upd = last_updated(room)
    total = sum(len(ph["items"][k]) for ph in phases for k, _ in KIND_TITLES)
    stamp = (f"数据更新于 data updated "
             f"{_time.strftime('%I:%M:%S %p', _time.localtime(upd))} · "
             f"{total} 条要点 points" if upd else "")
    if not phases:
        body = "<p>还没有已批准的洞察。No approved insights yet.</p>"
    else:
        parts = []
        for n, ph in enumerate(phases, 1):
            t = _time.strftime("%I:%M %p", _time.localtime(ph["start"])) \
                if ph["start"] else ""
            t = ("started 开始于 " + t) if t else ""
            parts.append(f"<h2>阶段 Phase {n}"
                         f"{' · ' + html.escape(ph['label']) if ph['label'] else ''}"
                         f" <small style='color:#7f8c99'>{t}</small></h2>")
            for kind, title_ in KIND_TITLES:
                items = ph["items"].get(kind, [])
                if not items:
                    continue
                parts.append(f"<h3>{html.escape(title_)}</h3><ul>")
                for it in items:
                    parts.append(
                        f"<li>{html.escape(it['zh'])}"
                        f"<br><span style='color:#a8b4c0;font-size:0.9em'>"
                        f"{html.escape(it['en'])}</span></li>")
                parts.append("</ul>")
        body = "".join(parts)
    head = ("<meta http-equiv=refresh content=30>"
            "<h1>本场会议至今 · Session so far</h1>"
            "<p style='color:#7f8c99'>已批准要点按话题阶段汇总，供主持人使用；"
            "大屏不显示本页。Approved points grouped by topic phase, for the "
            "moderator; not shown on the projector. 页面每 30 秒自动刷新。"
            "每条要点只出现一次，归入它首次出现的阶段与栏目，因此与实时面板的"
            "分栏可能不同。Each point appears once, in the phase and section "
            "where it first surfaced, so grouping can differ from the live "
            "panel.</p>"
            f"<p style='color:#86efac;font-size:0.95em'>{stamp}</p>")
    return _shell("Session so far", head + body)


def transcript_paths(room: str, session: str) -> tuple[Path, Path]:
    if session:
        base = Path(SESSIONS_DIR) / session
        return base / f"{room}_transcript.jsonl", \
            base / f"{room}_translations.jsonl"
    return Path(TRANSCRIPT_JSONL.format(room=room)), \
        Path(TRANSLATIONS_JSONL.format(room=room))


_T_CSS = (".seg{display:flex;gap:12px;margin-bottom:14px;align-items:baseline}"
          ".t{color:#556;font-size:13px;min-width:44px}"
          ".sp{font-weight:600;min-width:88px;font-size:14px}"
          ".lg{color:#556;font-size:12px;min-width:40px}"
          ".tx{flex:1;font-size:16px}"
          ".tr{color:#a8b8a8;font-size:14px;margin-top:2px}")
_COLORS = ["#6ea8fe", "#3ddc84", "#f0997b", "#ed93b1", "#facc15", "#a78bfa"]


def transcript_page(room: str, session: str) -> str:
    tpath, xpath = transcript_paths(room, session)
    trans = {}
    if xpath.exists():
        for i, line in enumerate(xpath.read_text().splitlines(), 1):
            e = json.loads(line)
            trans[e.get("id", i)] = e["translation"]
    rows = []
    if tpath.exists():
        for i, line in enumerate(tpath.read_text().splitlines(), 1):
            r = json.loads(line)
            mm, ss = divmod(int(r["t_start"]), 60)
            sp = r.get("speaker_id") or "?"
            c = _COLORS[(ord(sp[-1]) - 65) % len(_COLORS)]
            tr = trans.get(i, "")
            rows.append(
                f"<div class=seg><span class=t>{mm:02d}:{ss:02d}</span>"
                f"<span class=sp style='color:{c}'>{html.escape(sp)}</span>"
                f"<span class=lg>{html.escape(str(r.get('lang', '')))}</span>"
                f"<div class=tx>{html.escape(r.get('text', ''))}"
                + (f"<div class=tr>{html.escape(tr)}</div>" if tr else "")
                + "</div></div>")
    title = f"Transcript — {session or 'live'}"
    head = (f"<h1 style='font-size:19px;color:#7f8c99'>{html.escape(title)} · "
            f"<a style='color:#6ea8fe' href='/api/transcript?room="
            f"{html.escape(room)}&session={html.escape(session)}'>raw JSONL"
            "</a></h1>")
    return _shell(title, head + ("".join(rows) or "No transcript."), _T_CSS)
