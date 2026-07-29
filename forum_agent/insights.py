"""Insight engine (spec C4/C6): periodically (or on demand) feeds the recent
transcript + running state to the local LLM, producing structured bilingual
insights for the panel, and generates end-of-session minutes. Every item is
a DRAFT until the operator approves it (human-in-the-loop)."""
import json
import re
import threading
import time
import uuid
from pathlib import Path

import requests

from forum_agent.constants import (INSIGHT_INTERVAL_SECONDS, INSIGHT_MODEL,
                                   INSIGHT_THINK, INSIGHT_TIMEOUT_SECONDS,
                                   INSIGHT_WINDOW_SECONDS, INSIGHTS_JSON,
                                   MINUTES_MD, MSG_INSIGHTS, OLLAMA_URL,
                                   TRANSCRIPT_JSONL)
from forum_agent.server import hub

_PROMPTS = Path(__file__).resolve().parent.parent / "prompts"
KINDS = ["summary_points", "emerging_consensus", "tensions", "open_questions"]


def _llm(prompt: str) -> str:
    resp = requests.post(OLLAMA_URL, json={
        "model": INSIGHT_MODEL, "think": INSIGHT_THINK, "stream": False,
        "messages": [{"role": "user", "content": prompt}],
        "options": {"temperature": 0.2}}, timeout=INSIGHT_TIMEOUT_SECONDS)
    resp.raise_for_status()
    return resp.json()["message"]["content"].strip()


def _parse_json(text: str) -> dict:
    text = re.sub(r"^```(json)?|```$", "", text.strip(), flags=re.M).strip()
    start, end = text.find("{"), text.rfind("}")
    return json.loads(text[start:end + 1])


def read_transcript(room: str, window_s: float | None = None) -> str:
    path = Path(TRANSCRIPT_JSONL.format(room=room))
    if not path.exists():
        return ""
    records = [json.loads(l) for l in path.read_text().splitlines() if l]
    if window_s is not None and records:
        latest = records[-1]["t_end"]
        records = [r for r in records if r["t_end"] >= latest - window_s]
    return "\n".join(f"[{r['t_start']:.0f}-{r['t_end']:.0f}] "
                     f"{r['speaker_id']} ({r['lang']}): {r['text']}"
                     for r in records)


_engines: dict = {}
_engines_lock = threading.Lock()


def engine(room: str) -> "InsightEngine":
    with _engines_lock:
        if room not in _engines:
            _engines[room] = InsightEngine(room)
        return _engines[room]


class InsightEngine:
    def __init__(self, room: str) -> None:
        self.room = room
        self.path = Path(INSIGHTS_JSON.format(room=room))
        self.state = self._load()
        self._lock = threading.Lock()
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None
        self.error: str | None = None

    def reset(self) -> None:
        """New session = new meeting: clear previous insights so the panel
        never shows a past session's items as if they were current."""
        with self._lock:
            self.state = {"updated": 0, "items": {k: [] for k in KINDS},
                          "convergence_line": {"zh": "", "en": ""}}
            self._save_and_broadcast()

    def _load(self) -> dict:
        if self.path.exists():
            return json.loads(self.path.read_text())
        return {"updated": 0, "items": {k: [] for k in KINDS},
                "convergence_line": {"zh": "", "en": ""}}

    def _save_and_broadcast(self) -> None:
        self.path.parent.mkdir(exist_ok=True)
        self.path.write_text(json.dumps(self.state, ensure_ascii=False, indent=1))
        hub.broadcast_from_thread(self.room, {"type": MSG_INSIGHTS, **self.state})

    def refresh(self) -> dict:
        """One engine run: transcript window + state -> updated draft items.
        Operator-approved/hidden items keep their status across runs."""
        transcript = read_transcript(self.room, INSIGHT_WINDOW_SECONDS)
        if not transcript.strip():
            return self.state
        prompt = ((_PROMPTS / "insights.txt").read_text()
                  .replace("{state}",
                           json.dumps(self.state["items"], ensure_ascii=False))
                  .replace("{transcript}", transcript))
        parsed = _parse_json(_llm(prompt))
        with self._lock:
            for kind in KINDS:
                fresh = parsed.get(kind, []) or []
                kept = {i["zh"]: i for i in self.state["items"][kind]
                        if i["status"] != "draft"}  # approvals/hides survive
                items = list(kept.values())
                for it in fresh:
                    if isinstance(it, dict) and it.get("zh") and \
                            it["zh"] not in kept:
                        items.append({"id": uuid.uuid4().hex[:8],
                                      "zh": it["zh"], "en": it.get("en", ""),
                                      "status": "draft"})
                self.state["items"][kind] = items
            line = parsed.get("convergence_line") or {}
            if isinstance(line, dict) and line.get("zh"):
                self.state["convergence_line"] = {"zh": line["zh"],
                                                  "en": line.get("en", "")}
            self.state["updated"] = time.time()
            self._save_and_broadcast()
        return self.state

    def set_item(self, item_id: str, action: str, zh: str = "",
                 en: str = "") -> dict:
        """Operator console: approve / hide / edit a single item."""
        with self._lock:
            for kind in KINDS:
                for it in self.state["items"][kind]:
                    if it["id"] == item_id:
                        if action == "edit":
                            it["zh"], it["en"] = zh or it["zh"], en or it["en"]
                            it["status"] = "approved"
                        elif action in ("approved", "hidden", "draft"):
                            it["status"] = action
            self._save_and_broadcast()
        return self.state

    def start_auto(self) -> None:
        self._stop = threading.Event()
        stop = self._stop

        def _loop() -> None:
            while not stop.wait(INSIGHT_INTERVAL_SECONDS):
                try:
                    self.refresh()
                    self.error = None
                except Exception as exc:  # surfaced on the console via status
                    self.error = f"insights: {type(exc).__name__}: {exc}"
                    print(f"[insights] refresh failed: {exc!r}")

        self._thread = threading.Thread(target=_loop, daemon=True)
        self._thread.start()

    def stop_auto(self) -> None:
        self._stop.set()

    def generate_minutes(self) -> str:
        """C6: end-of-session bilingual draft minutes as Markdown."""
        transcript = read_transcript(self.room)
        approved = {k: [i for i in self.state["items"][k]
                        if i["status"] == "approved"] for k in KINDS}
        prompt = ((_PROMPTS / "minutes.txt").read_text()
                  .replace("{insights}",
                           json.dumps(approved, ensure_ascii=False))
                  .replace("{transcript}", transcript))
        md = _llm(prompt)
        md = re.sub(r"^```(markdown)?|```$", "", md.strip(), flags=re.M).strip()
        out = Path(MINUTES_MD.format(room=self.room))
        out.write_text(f"> DRAFT — pending human review / 草稿，待人工确认\n\n{md}\n")
        return str(out)
