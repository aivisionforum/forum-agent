"""Report drafter (spec C7): synthesizes all archived sessions into an
anonymized bilingual event-report draft. Batch job — runs after the event
(or any time), so it can afford the larger, slower model."""
import json
import re
import time
from pathlib import Path

import requests

from forum_agent.constants import (OLLAMA_URL, REPORT_MD, REPORT_MODEL,
                                   REPORT_TIMEOUT_SECONDS, SESSIONS_DIR)

_PROMPTS = Path(__file__).resolve().parent.parent / "prompts"
MAX_MINUTES_CHARS = 4000  # per session, keep total prompt within context


def _llm(prompt: str) -> str:
    resp = requests.post(OLLAMA_URL, json={
        "model": REPORT_MODEL, "stream": False,
        "messages": [{"role": "user", "content": prompt}],
        "options": {"temperature": 0.3}}, timeout=REPORT_TIMEOUT_SECONDS)
    resp.raise_for_status()
    return resp.json()["message"]["content"].strip()


def _session_block(d: Path, room: str = "room1") -> str | None:
    """One session's material: title + approved insights + minutes."""
    meta = d / "meta.json"
    title = ""
    if meta.exists():
        try:
            title = json.loads(meta.read_text()).get("title", "")
        except json.JSONDecodeError:
            pass
    parts = [f"### Session {d.name}: {title or 'untitled'}"]
    ins = d / f"{room}_insights.json"
    if ins.exists():
        data = json.loads(ins.read_text())
        log = data.get("approved_log") or data.get("items") or {}
        parts.append("Approved insights: "
                     + json.dumps(log, ensure_ascii=False))
    minutes = d / f"{room}_minutes.md"
    if minutes.exists():
        parts.append("Minutes:\n" + minutes.read_text()[:MAX_MINUTES_CHARS])
    if len(parts) == 1:
        return None  # nothing usable for the report
    return "\n".join(parts)


def generate_report() -> str:
    """Draft the event synthesis report from every archived session."""
    root = Path(SESSIONS_DIR)
    blocks = []
    if root.exists():
        for d in sorted(p for p in root.iterdir() if p.is_dir()):
            block = _session_block(d)
            if block:
                blocks.append(block)
    if not blocks:
        raise RuntimeError("no archived sessions with insights or minutes")
    prompt = (_PROMPTS / "report.txt").read_text().replace(
        "{sessions}", "\n\n".join(blocks))
    md = _llm(prompt)
    md = re.sub(r"^```(markdown)?|```$", "", md.strip(), flags=re.M).strip()
    content = ("> DRAFT — pending human review / 草稿，待组委会审定\n"
               f"> generated {time.strftime('%Y-%m-%d %H:%M')} from "
               f"{len(blocks)} session(s)\n\n{md}\n")
    out = Path(REPORT_MD)
    out.parent.mkdir(exist_ok=True)
    out.write_text(content)
    out.with_name(out.name.replace(".md", time.strftime(
        "_%Y%m%d-%H%M%S.md"))).write_text(content)  # never overwrite history
    return str(out)
