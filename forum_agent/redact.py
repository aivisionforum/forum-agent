"""Post-session name check (issue #10). Chatham House anonymises speaker
labels, but the transcript still contains every name spoken aloud. A local
LLM lists the personal names it finds; a human edits the files. This module
never modifies a transcript itself — the report is advisory, like every
other AI output here."""
import json
import re
from pathlib import Path

from forum_agent import llm
from forum_agent.constants import SESSIONS_DIR, TRANSLATE_MODEL

_PROMPT = Path(__file__).resolve().parent.parent.joinpath(
    "prompts/redact.txt").read_text()
REDACTION_MD = "redaction_report.md"


def check_session(session_id: str) -> Path:
    """Scan an archived session's transcripts for personal names and write
    redaction_report.md into the session folder. Returns the report path.
    Raises RuntimeError when the session has no transcript."""
    sdir = Path(SESSIONS_DIR) / session_id
    lines = []
    for t in sorted(sdir.glob("*_transcript.jsonl")):
        for raw in t.read_text().splitlines():
            try:
                lines.append(json.loads(raw).get("text", ""))
            except json.JSONDecodeError:
                lines.append(raw)  # keep numbering aligned with the file
    if not lines:
        raise RuntimeError(f"session {session_id} has no transcript")
    numbered = "\n".join(f"{i}: {t}" for i, t in enumerate(lines))
    reply = llm.chat(TRANSLATE_MODEL, _PROMPT.replace("{transcript}", numbered),
                     temperature=0.0, timeout=300)
    names = _parse_names(reply)
    report = sdir / REDACTION_MD
    report.write_text(_render(session_id, names, lines))
    return report


def _parse_names(reply: str) -> list[dict]:
    """The model is told 'strict JSON'; tolerate fenced or prefixed output.
    An unparseable reply is an error, not an empty (clean) result."""
    m = re.search(r"\{.*\}", reply, re.S)
    if not m:
        raise RuntimeError(f"redaction model returned no JSON: {reply[:200]!r}")
    return json.loads(m.group(0)).get("names", [])


def _render(session_id: str, names: list[dict], lines: list[str]) -> str:
    out = [f"# Name check — session {session_id}", "",
           "DRAFT — advisory list from a local LLM. Review each item, then "
           "edit the transcript/minutes by hand where redaction is needed. "
           "Nothing has been changed automatically.", ""]
    if not names:
        out.append("No personal names found.")
        return "\n".join(out) + "\n"
    for n in names:
        out.append(f"## {n.get('name', '?')}")
        for i in n.get("lines", []):
            if isinstance(i, int) and 0 <= i < len(lines):
                out.append(f"- line {i}: {lines[i]}")
        out.append("")
    return "\n".join(out) + "\n"
