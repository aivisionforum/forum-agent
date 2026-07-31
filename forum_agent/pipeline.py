"""Pipeline: per-session workers turning utterance audio into transcript
records, subtitles, and translations. Owned by replay.run_mic/run_replay."""
import json
import queue
import threading
import time
from pathlib import Path

from forum_agent import asr, translate
from forum_agent.constants import (MSG_FINAL, MSG_PARTIAL, MSG_TRANSLATION,
                                   SAMPLE_RATE, TRANSCRIPT_JSONL,
                                   TRANSLATIONS_JSONL)
from forum_agent.diarize import Diarizer
from forum_agent.server import hub


class Pipeline:
    def __init__(self, room: str) -> None:
        self.room = room
        self.diarizer = Diarizer()
        self.jsonl = Path(TRANSCRIPT_JSONL.format(room=room))
        self.jsonl.parent.mkdir(exist_ok=True)
        self.jsonl.write_text("")
        self.translate_q: queue.Queue = queue.Queue()
        self.trans_jsonl = Path(TRANSLATIONS_JSONL.format(room=room))
        self.trans_jsonl.write_text("")
        self.lags: list[float] = []
        self.seq = 0
        self.final_q: queue.Queue = queue.Queue()
        self._partial_slot: tuple | None = None  # latest open-segment snapshot
        self._slot_lock = threading.Lock()
        self.idle = threading.Event()  # set when no final work is pending
        self.idle.set()
        self.closed = threading.Event()  # workers exit; late results dropped
        threading.Thread(target=self._translator, daemon=True).start()
        threading.Thread(target=self._asr_worker, daemon=True).start()

    def close(self) -> None:
        """End of session: stop both workers. Without this, every session
        leaked two immortal threads and a stale translator could append the
        previous meeting's translations into the next session's files."""
        self.closed.set()

    def warmup(self) -> None:
        """Load every model (Whisper, ECAPA, Ollama) before the replay clock
        starts; cold loads mid-stream showed up as 10s+ subtitle lag spikes."""
        import numpy as np
        asr.warmup()
        self.diarizer._embed(np.random.default_rng(0)
                             .normal(0, 0.1, SAMPLE_RATE).astype("float32"))
        translate.translate("warmup", "en")

    def submit_final(self, t_start: float, audio, wall_offset: float) -> None:
        self.idle.clear()
        self.final_q.put((t_start, audio, wall_offset))

    def submit_partial(self, t_start: float, audio, wall_offset: float) -> None:
        with self._slot_lock:  # overwrite: only the freshest partial matters
            self._partial_slot = (t_start, audio, wall_offset)

    def _asr_worker(self) -> None:
        """Finals take priority; partials are best-effort on idle cycles, so
        the replay feed thread never blocks on Whisper or ECAPA."""
        while not self.closed.is_set():
            try:
                job = self.final_q.get(timeout=0.1)
                try:
                    self.on_final(*job)
                except Exception as exc:  # keep worker alive; skip bad segment
                    print(f"[asr-worker] final failed, segment dropped: {exc!r}")
                if self.final_q.empty():
                    self.idle.set()
                continue
            except queue.Empty:
                pass
            with self._slot_lock:
                job, self._partial_slot = self._partial_slot, None
            if job is not None:
                try:
                    self.on_partial(*job)
                except Exception as exc:  # partials are best-effort anyway
                    print(f"[asr-worker] partial skipped: {exc!r}")

    def _translator(self) -> None:
        while not self.closed.is_set():
            try:
                seg_id, text, lang = self.translate_q.get(timeout=0.5)
            except queue.Empty:
                continue
            translation = translate.translate(text, lang)
            if translation and not self.closed.is_set():
                with self.trans_jsonl.open("a") as f:
                    f.write(json.dumps({"id": seg_id, "translation": translation},
                                       ensure_ascii=False) + "\n")
                hub.broadcast_from_thread(self.room, {
                    "type": MSG_TRANSLATION, "id": seg_id,
                    "translation": translation})

    def on_partial(self, t_start: float, audio, wall_offset: float) -> None:
        text, lang = asr.transcribe(audio)
        if not text:
            return
        self._track_lag(t_start + len(audio) / SAMPLE_RATE, wall_offset)
        hub.broadcast_from_thread(self.room, {
            "type": MSG_PARTIAL, "t_start": t_start, "lang": lang,
            "text": text})

    def on_final(self, t_start: float, audio, wall_offset: float) -> None:
        text, lang = asr.transcribe(audio)
        if not text:
            return
        t_end = t_start + len(audio) / SAMPLE_RATE
        speaker = self.diarizer.assign(audio)
        self.seq += 1
        record = {"t_start": round(t_start, 2), "t_end": round(t_end, 2),
                  "speaker_id": speaker, "lang": lang, "text": text}
        with self.jsonl.open("a") as f:
            f.write(json.dumps(record, ensure_ascii=False) + "\n")
        lag = self._track_lag(t_end, wall_offset)
        hub.broadcast_from_thread(self.room, {
            "type": MSG_FINAL, "id": self.seq, **record, "translation": ""})
        print(f"[final #{self.seq}] {speaker} {lang} lag={lag:.1f}s: {text[:60]}")
        self.translate_q.put((self.seq, text, lang))

    def _track_lag(self, audio_time: float, wall_offset: float) -> float:
        lag = (time.monotonic() - wall_offset) - audio_time
        self.lags.append(lag)
        return lag
