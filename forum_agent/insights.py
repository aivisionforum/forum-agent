"""Insight engine (spec C4/C6): periodically (or on demand) feeds the recent
transcript + running state to the local LLM, producing structured bilingual
insights for the panel, and generates end-of-session minutes.

Review modes (human agency, two flavors): auto-approve (default) shows items
immediately and the operator corrects/hides on review — human-on-the-loop;
gatekeeper mode holds every item as DRAFT until approved."""
import json
import re
import threading
import time
import uuid
from pathlib import Path

from forum_agent import llm
from forum_agent.constants import (AUTO_APPROVE_DEFAULT, INSIGHT_MAX_ITEMS,
                                   INSIGHTS_HISTORY,
                                   INSIGHT_INTERVAL_SECONDS, INSIGHT_MODEL,
                                   INSIGHT_THINK, INSIGHT_TIMEOUT_SECONDS,
                                   INSIGHT_WINDOW_SECONDS, INSIGHTS_JSON,
                                   MINUTES_MD, MSG_INSIGHTS,
                                   TRANSCRIPT_JSONL)
from forum_agent.server import hub

_PROMPTS = Path(__file__).resolve().parent.parent / "prompts"
def _tracked(label):
    """Register the wrapped engine method in the activity registry so every
    page can show the user what is being generated (and for how long)."""
    import functools

    def deco(fn):
        @functools.wraps(fn)
        def wrap(*a, **k):
            from forum_agent import activity
            with activity.task(label):
                return fn(*a, **k)
        return wrap
    return deco


KINDS = ["summary_points", "next_steps", "emerging_consensus", "tensions",
         "open_questions"]


def _llm(prompt: str, big: bool = False) -> str:
    """big=True: post-session work (archived insights, minutes) — the GPU is
    free then, so the 32B report model's quality is affordable (issue #13).
    Live refresh stays on the 8B (ADR 0001: never starve translation)."""
    if big:
        from forum_agent.constants import (REPORT_MODEL,
                                           REPORT_TIMEOUT_SECONDS)
        return llm.chat(REPORT_MODEL, prompt,
                        timeout=REPORT_TIMEOUT_SECONDS)
    return llm.chat(INSIGHT_MODEL, prompt, think=INSIGHT_THINK,
                    timeout=INSIGHT_TIMEOUT_SECONDS)


def _norm(text: str) -> str:
    """Normalization for quote grounding: strip whitespace/punct, casefold."""
    return re.sub(r"[\W_]+", "", text, flags=re.UNICODE).casefold()


def _is_grounded(item: dict, transcript_norm: str) -> bool:
    """An item is grounded when its verbatim quote actually appears in the
    transcript window (issue #12: hallucinated points must not auto-approve)."""
    q = _norm(item.get("quote", "") or "")
    return len(q) >= 4 and q in transcript_norm


def _parse_json(text: str) -> dict:
    text = re.sub(r"^```(json)?|```$", "", text.strip(), flags=re.M).strip()
    start, end = text.find("{"), text.rfind("}")
    return json.loads(text[start:end + 1])


def read_transcript(room: str, window_s: float | None = None,
                    base_dir: str | None = None) -> str:
    path = (Path(base_dir) / f"{room}_transcript.jsonl") if base_dir \
        else Path(TRANSCRIPT_JSONL.format(room=room))
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
        self.auto_approve = AUTO_APPROVE_DEFAULT
        self.auto_started: float | None = None  # panel countdown to 1st run
        self._gen = 0  # bumped on reset/stop: in-flight refreshes discard

    def reset(self) -> None:
        """New session = new meeting: clear previous insights so the panel
        never shows a past session's items as if they were current."""
        with self._lock:
            self._gen += 1
            self.state = {"updated": 0, "items": {k: [] for k in KINDS},
                          "convergence_line": {"zh": "", "en": ""},
                          "hidden_zh": [], "approved_log": {k: [] for k in KINDS}}
            self._save_and_broadcast()

    def _load(self) -> dict:
        if self.path.exists():
            return json.loads(self.path.read_text())
        return {"updated": 0, "items": {k: [] for k in KINDS},
                "convergence_line": {"zh": "", "en": ""},
                "hidden_zh": [], "approved_log": {k: [] for k in KINDS}}

    def _log_approved(self, kind: str, item: dict) -> None:
        """Persistent record of everything ever approved/shown: the minutes
        and report consume this, so items rotating off the live panel are
        still counted."""
        log = self.state.setdefault("approved_log", {k: [] for k in KINDS})
        if not any(e["zh"] == item["zh"] for e in log.setdefault(kind, [])):
            log[kind].append({"zh": item["zh"], "en": item["en"]})

    def _save_and_broadcast(self) -> None:
        self.path.parent.mkdir(exist_ok=True)
        self.path.write_text(json.dumps(self.state, ensure_ascii=False, indent=1))
        hub.broadcast_from_thread(self.room, {"type": MSG_INSIGHTS, **self.state})

    def latest_archive_dir(self) -> str | None:
        import forum_agent.session as fs
        sessions = fs.list_sessions()
        if not sessions:
            return None
        # fs.SESSIONS_DIR (not constants) so both resolve identically
        return f"{fs.SESSIONS_DIR}/{sessions[0]['id']}"

    @_tracked("summarizing insights (~30-60s)")
    def refresh(self) -> dict:
        """One engine run: transcript window + state -> updated draft items.
        With no live session, falls back to the latest archived session and
        writes the insights there (same policy as minutes) — short meetings
        can be summarized after the fact."""
        base_dir = None
        transcript = read_transcript(self.room, INSIGHT_WINDOW_SECONDS)
        if not transcript.strip():
            from forum_agent.session import manager
            if manager.status()["running"]:
                # live session but nothing transcribed yet: never fall back
                # to the previous meeting's archive mid-session
                raise RuntimeError("no speech transcribed yet — speak first, "
                                   "then summarize")
            base_dir = self.latest_archive_dir()
            if base_dir:
                transcript = read_transcript(self.room, base_dir=base_dir)
        if not transcript.strip():
            raise RuntimeError("no transcript available (live or archived)")
        if base_dir:
            return self._refresh_archived(base_dir, transcript)
        prompt = ((_PROMPTS / "insights.txt").read_text()
                  .replace("{state}",
                           json.dumps(self.state["items"], ensure_ascii=False))
                  .replace("{transcript}", transcript))
        gen = self._gen
        parsed = _parse_json(_llm(prompt))
        now = time.time()
        with self._lock:
            stale = gen != self._gen
        if stale:
            # The session was stopped (archived) or reset during the LLM
            # call. The result belongs to THAT meeting: store it into the
            # latest archive instead of contaminating the new session or
            # discarding the operator's work.
            base_dir = self.latest_archive_dir()
            if base_dir:
                empty = {"updated": 0, "items": {k: [] for k in KINDS},
                         "convergence_line": {"zh": "", "en": ""},
                         "hidden_zh": [],
                         "approved_log": {k: [] for k in KINDS}}
                apath = Path(base_dir) / f"{self.room}_insights.json"
                prev = json.loads(apath.read_text()) if apath.exists() \
                    else empty
                return self._store_archived(base_dir, prev, parsed)
            return self.state
        with self._lock:
            hidden = set(self.state.get("hidden_zh", []))
            tnorm = _norm(transcript)
            for kind in KINDS:
                fresh = parsed.get(kind, []) or []
                prev = {i["zh"]: i
                        for i in self.state["items"].get(kind, [])}
                # The panel is a bounded live snapshot: each refresh REPLACES
                # the section with the LLM's current best items. Superseded
                # items retire (history + approved_log keep them); operator
                # hides stay suppressed even if re-suggested.
                items = []
                for it in fresh:
                    if not (isinstance(it, dict) and it.get("zh")) \
                            or it["zh"] in hidden:
                        continue
                    old = prev.get(it["zh"])
                    grounded = _is_grounded(it, tnorm)
                    # ungrounded (no verbatim source in the transcript) stays
                    # DRAFT even with auto-approve on: a human must vouch for
                    # anything the model cannot anchor (issue #12)
                    items.append(old or {
                        "id": uuid.uuid4().hex[:8],
                        "zh": it["zh"], "en": it.get("en", ""),
                        "quote": it.get("quote", ""), "grounded": grounded,
                        "status": "approved"
                        if self.auto_approve and grounded else "draft",
                        "added": now})
                self.state["items"][kind] = items[:INSIGHT_MAX_ITEMS[kind]]
                if self.auto_approve:  # supervisor mode: log for the minutes
                    for it in self.state["items"].get(kind, []):
                        self._log_approved(kind, it)
            line = parsed.get("convergence_line") or {}
            if isinstance(line, dict) and line.get("zh"):
                self.state["convergence_line"] = {"zh": line["zh"],
                                                  "en": line.get("en", "")}
            topic = parsed.get("session_topic") or {}
            if isinstance(topic, dict) and topic.get("zh"):
                self.state["session_topic"] = {"zh": topic["zh"],
                                               "en": topic.get("en", "")}
            self.state["updated"] = time.time()
            self._save_and_broadcast()
            with Path(INSIGHTS_HISTORY.format(room=self.room)).open("a") as f:
                f.write(json.dumps(self.state, ensure_ascii=False) + "\n")
        return self.state

    @_tracked("summarizing insights (~30-60s)")
    def refresh_for(self, session_id: str) -> dict:
        """Insights for one specific archived session (operator-selected)."""
        from forum_agent.constants import SESSIONS_DIR
        base_dir = f"{SESSIONS_DIR}/{session_id}"
        transcript = read_transcript(self.room, base_dir=base_dir)
        if not transcript.strip():
            raise RuntimeError(f"session {session_id}: no transcript")
        return self._refresh_archived(base_dir, transcript)

    @_tracked("generating minutes (~1-2 min)")
    def minutes_for(self, session_id: str) -> str:
        """Minutes for one specific archived session, using that session's
        own approved insights (never the live room state)."""
        from forum_agent.constants import SESSIONS_DIR
        base_dir = f"{SESSIONS_DIR}/{session_id}"
        transcript = read_transcript(self.room, base_dir=base_dir)
        if not transcript.strip():
            raise RuntimeError(f"session {session_id}: no transcript")
        apath = Path(base_dir) / f"{self.room}_insights.json"
        approved = {}
        if apath.exists():
            data = json.loads(apath.read_text())
            approved = data.get("approved_log") or data.get("items") or {}
        prompt = ((_PROMPTS / "minutes.txt").read_text()
                  .replace("{insights}",
                           json.dumps(approved, ensure_ascii=False))
                  .replace("{transcript}", transcript))
        md = _llm(prompt, big=True)
        md = re.sub(r"^```(markdown)?|```$", "", md.strip(),
                    flags=re.M).strip()
        out = Path(base_dir) / f"{self.room}_minutes.md"
        content = ("> DRAFT — pending human review / 草稿，待人工确认\n\n"
                   f"{md}\n")
        out.write_text(content)
        stamped = out.with_name(out.name.replace(
            ".md", time.strftime("_%H%M%S.md")))
        stamped.write_text(content)
        return str(out)

    def _refresh_archived(self, base_dir: str, transcript: str) -> dict:
        """Post-hoc insights for an already-archived session: reads and
        writes that session's own insights file; live state untouched."""
        apath = Path(base_dir) / f"{self.room}_insights.json"
        state = json.loads(apath.read_text()) if apath.exists() else \
            {"updated": 0, "items": {k: [] for k in KINDS},
             "convergence_line": {"zh": "", "en": ""},
             "hidden_zh": [], "approved_log": {k: [] for k in KINDS}}
        prompt = ((_PROMPTS / "insights.txt").read_text()
                  .replace("{state}",
                           json.dumps(state["items"], ensure_ascii=False))
                  .replace("{transcript}", transcript))
        parsed = _parse_json(_llm(prompt, big=True))
        return self._store_archived(base_dir, state, parsed,
                                    transcript=transcript)

    def _store_archived(self, base_dir: str, state: dict,
                        parsed: dict, transcript: str = "") -> dict:
        import uuid as _uuid
        apath = Path(base_dir) / f"{self.room}_insights.json"
        now = time.time()
        tnorm = _norm(transcript)
        for kind in KINDS:
            items = []
            for it in (parsed.get(kind, []) or []):
                if not (isinstance(it, dict) and it.get("zh")):
                    continue
                grounded = not transcript or _is_grounded(it, tnorm)
                items.append({"id": _uuid.uuid4().hex[:8], "zh": it["zh"],
                              "en": it.get("en", ""),
                              "quote": it.get("quote", ""),
                              "grounded": grounded,
                              "status": "approved" if grounded else "draft",
                              "added": now})
            state["items"][kind] = items[:INSIGHT_MAX_ITEMS[kind]]
            state["approved_log"][kind] = [
                {"zh": i["zh"], "en": i["en"]}
                for i in state["items"][kind] if i["status"] == "approved"]
        for key in ("convergence_line", "session_topic"):
            val = parsed.get(key) or {}
            if isinstance(val, dict) and val.get("zh"):
                state[key] = {"zh": val["zh"], "en": val.get("en", "")}
        state["updated"] = now
        apath.write_text(json.dumps(state, ensure_ascii=False, indent=1))
        return {**state, "archived": Path(base_dir).name}

    def set_item(self, item_id: str, action: str, zh: str = "",
                 en: str = "") -> dict:
        """Operator console: approve / hide / edit a single item."""
        with self._lock:
            for kind in KINDS:
                for it in self.state["items"].get(kind, []):
                    if it["id"] != item_id:
                        continue
                    if action == "edit":
                        old_zh = it["zh"]
                        it["zh"], it["en"] = zh or it["zh"], en or it["en"]
                        it["status"] = "approved"
                        log = self.state.get("approved_log", {}).get(kind, [])
                        log[:] = [e for e in log if e["zh"] != old_zh]
                        self._log_approved(kind, it)
                    elif action in ("approved", "hidden", "draft"):
                        it["status"] = action
                        if action == "approved":
                            self._log_approved(kind, it)
                        else:  # demote (draft) or hide: out of the minutes log
                            log = self.state.get("approved_log", {}).get(kind, [])
                            log[:] = [e for e in log if e["zh"] != it["zh"]]
                        if action == "hidden":  # stays suppressed on refresh
                            self.state.setdefault("hidden_zh", []).append(it["zh"])
                        else:  # approve/unhide lifts refresh suppression too
                            hid = self.state.get("hidden_zh", [])
                            hid[:] = [z for z in hid if z != it["zh"]]
            self._save_and_broadcast()
        return self.state

    def start_auto(self) -> None:
        self.auto_started = time.time()
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
        self.auto_started = None
        self._stop.set()
        with self._lock:
            self._gen += 1  # invalidate any refresh still in the LLM

    @_tracked("generating minutes (~1-2 min)")
    def generate_minutes(self) -> str:
        """C6: end-of-session bilingual draft minutes as Markdown. If the
        session was already archived (Stop moves the live files), the minutes
        are generated from and written into the latest archive."""
        base_dir = None
        transcript = read_transcript(self.room)
        if not transcript.strip():
            from forum_agent.session import list_sessions
            sessions = list_sessions()
            if sessions:
                from forum_agent.constants import SESSIONS_DIR
                base_dir = f"{SESSIONS_DIR}/{sessions[0]['id']}"
                transcript = read_transcript(self.room, base_dir=base_dir)
        if not transcript.strip():
            raise RuntimeError("no transcript available for minutes")
        approved = self.state.get("approved_log") or {
            k: [i for i in self.state["items"].get(k, [])
                if i["status"] == "approved"] for k in KINDS}
        prompt = ((_PROMPTS / "minutes.txt").read_text()
                  .replace("{insights}",
                           json.dumps(approved, ensure_ascii=False))
                  .replace("{transcript}", transcript))
        from forum_agent.session import manager
        # mid-session minutes must not put the 32B on the GPU that live
        # translation is using; post-session gets the big model
        md = _llm(prompt, big=not manager.status()["running"])
        md = re.sub(r"^```(markdown)?|```$", "", md.strip(), flags=re.M).strip()
        out = (Path(base_dir) / f"{self.room}_minutes.md") if base_dir \
            else Path(MINUTES_MD.format(room=self.room))
        content = f"> DRAFT — pending human review / 草稿，待人工确认\n\n{md}\n"
        out.write_text(content)
        # regenerations must not destroy earlier drafts: timestamped copy too
        stamped = out.with_name(out.name.replace(
            ".md", time.strftime("_%H%M%S.md")))
        stamped.write_text(content)
        return str(out)
