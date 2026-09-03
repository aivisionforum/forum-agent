"""Operator 'session so far' view (issue #17): the live insight panel is a
bounded snapshot of the CURRENT topic, so in a meeting that spans several
areas the moderator has no single view of everything covered. This module
groups the session's approved points into topic phases from the append-only
insights history. Read-only; the minutes remain the canonical aggregate."""
import json
from pathlib import Path

from forum_agent.constants import INSIGHTS_HISTORY

KIND_TITLES = [
    ("summary_points", "要点摘要 · Key points"),
    ("next_steps", "行动项 · Next steps"),
    ("emerging_consensus", "正在形成的共识 · Emerging consensus"),
    ("tensions", "主要分歧 · Tensions"),
    ("open_questions", "待回答的问题 · Open questions"),
]


def build(room: str, base_dir: str | None = None) -> list[dict]:
    """Phases of the session: a new phase starts when the convergence line
    changes; each approved point appears once, in the phase where it first
    surfaced."""
    path = (Path(base_dir) / f"{room}_insights_history.jsonl") if base_dir \
        else Path(INSIGHTS_HISTORY.format(room=room))
    if not path.exists():
        return []
    phases: list[dict] = []
    seen: set[str] = set()
    for line in path.read_text().splitlines():
        try:
            snap = json.loads(line)
        except json.JSONDecodeError:
            continue  # a torn write must not blank the whole view
        label = (snap.get("convergence_line") or {}).get("zh") or ""
        if not phases or (label and label != phases[-1]["label"]):
            phases.append({"label": label, "start": snap.get("updated", 0),
                           "items": {k: [] for k, _ in KIND_TITLES}})
        elif label:
            phases[-1]["label"] = label
        for kind, _ in KIND_TITLES:
            for it in snap.get("items", {}).get(kind, []):
                zh = it.get("zh")
                if it.get("status") == "approved" and zh and zh not in seen:
                    seen.add(zh)
                    phases[-1]["items"][kind].append(
                        {"zh": zh, "en": it.get("en", "")})
    return [p for p in phases
            if any(p["items"][k] for k, _ in KIND_TITLES)]
