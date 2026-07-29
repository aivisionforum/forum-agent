"""M1 replay orchestrator: one-command startup.

    python -m forum_agent.replay data/fixture_meeting.wav --room room1

Starts the subtitle server, then replays the WAV pinned to wall-clock time
(simulating a live feed), running VAD -> Whisper -> diarization -> JSONL ->
websocket, with translation on a separate worker so subtitles never wait.
"""
import argparse
import json
import queue
import threading
import time
from pathlib import Path

import soundfile as sf
import uvicorn

from forum_agent import asr, translate
from forum_agent.constants import (DEAD_STREAM_SECONDS, FRAME_SECONDS,
                                   MSG_FINAL, MSG_PARTIAL,
                                   MSG_TRANSLATION, PARTIAL_INTERVAL_SECONDS,
                                   SAMPLE_RATE, SERVER_HOST, SERVER_PORT,
                                   TRANSCRIPT_JSONL)
from forum_agent.diarize import Diarizer
from forum_agent.segmenter import Segmenter
from forum_agent.server import app, hub


class Pipeline:
    def __init__(self, room: str) -> None:
        self.room = room
        self.diarizer = Diarizer()
        self.jsonl = Path(TRANSCRIPT_JSONL.format(room=room))
        self.jsonl.parent.mkdir(exist_ok=True)
        self.jsonl.write_text("")
        self.translate_q: queue.Queue = queue.Queue()
        self.trans_jsonl = self.jsonl.with_name(f"{room}_translations.jsonl")
        self.trans_jsonl.write_text("")
        self.lags: list[float] = []
        self.seq = 0
        self.final_q: queue.Queue = queue.Queue()
        self._partial_slot: tuple | None = None  # latest open-segment snapshot
        self._slot_lock = threading.Lock()
        self.idle = threading.Event()  # set when no final work is pending
        self.idle.set()
        threading.Thread(target=self._translator, daemon=True).start()
        threading.Thread(target=self._asr_worker, daemon=True).start()

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
        while True:
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
        while True:
            seg_id, text, lang = self.translate_q.get()
            translation = translate.translate(text, lang)
            if translation:
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


def feed_frame(pipe: Pipeline, seg: Segmenter, frame, start: float,
               next_partial: float) -> float:
    """Push one frame through VAD; submit finals/partials. Returns updated
    next-partial deadline."""
    for closed in seg.feed(frame):
        pipe.submit_final(closed.t_start, closed.audio, start)
        next_partial = seg._clock + PARTIAL_INTERVAL_SECONDS
    cur = seg.open_segment
    if cur is not None and seg._clock >= next_partial:
        pipe.submit_partial(cur.t_start, cur.audio.copy(), start)
        next_partial = seg._clock + PARTIAL_INTERVAL_SECONDS
    return next_partial


def run_mic(room: str, duration: float | None = None,
            stop_event: threading.Event | None = None,
            on_phase=None, device: int | None = None) -> Pipeline:
    """Live capture from the default input device (MacBook mic or a USB
    mixer feed) through the same pipeline as replay. Uses the Silero neural
    VAD: no noise-floor calibration or sensitivity tuning needed."""
    import sounddevice as sd
    from forum_agent.vad import AutoGain, SileroSpeech
    phase = on_phase or (lambda p: None)
    phase("loading models")
    pipe = Pipeline(room)
    pipe.warmup()
    seg = Segmenter(speech_fn=SileroSpeech())
    agc = AutoGain()
    frame_len = int(FRAME_SECONDS * SAMPLE_RATE)
    import numpy as np
    start = time.monotonic()
    next_partial = PARTIAL_INTERVAL_SECONDS
    dead_frames = 0
    max_dead = int(DEAD_STREAM_SECONDS / FRAME_SECONDS)

    def _done() -> bool:
        return ((duration is not None and time.monotonic() - start >= duration)
                or (stop_event is not None and stop_event.is_set()))

    def _pick_device():
        """Explicit user choice wins; otherwise prefer the built-in mic:
        macOS silently makes AirPods/Continuity devices the default input,
        which killed live sessions (wrong room, dead zeros, BT overflows)."""
        if device is not None:
            return device, sd.query_devices(device)["name"]
        for i, d in enumerate(sd.query_devices()):
            if d["max_input_channels"] > 0 and "MacBook" in d["name"] \
                    and "Microphone" in d["name"]:
                return i, d["name"]
        d = sd.query_devices(kind="input")
        return None, d["name"]  # fall back to system default

    try:
        while not _done():
            # (Re)open the CURRENT default input device: macOS can switch
            # devices mid-session (Continuity/AirPods) leaving the old
            # stream delivering pure zeros; the watchdog below reopens.
            device, dev_name = _pick_device()
            with sd.InputStream(samplerate=SAMPLE_RATE, channels=1,
                                dtype="float32", blocksize=frame_len,
                                device=device) as stream:
                print(f"Mic live: {dev_name} "
                      "(Silero VAD + auto-gain). Ctrl-C to stop.")
                phase("live")
                raw_peak, next_beat = 0.0, time.monotonic() - start + 10.0
                while not _done():
                    raw, overflowed = stream.read(frame_len)
                    raw = raw[:, 0]
                    if overflowed:
                        print("[mic] input overflow: audio dropped by OS")
                    peak = float(np.max(np.abs(raw)))
                    raw_peak = max(raw_peak, peak)
                    dead_frames = dead_frames + 1 if peak == 0.0 else 0
                    if dead_frames >= max_dead:
                        print("[mic] stream dead (all zeros); reopening "
                              "default input device")
                        dead_frames = 0
                        break  # exits inner loop -> reopens stream
                    if time.monotonic() - start >= next_beat:
                        print(f"[mic] raw peak {raw_peak:.4f} over last 10s "
                              f"(speaking should reach > 0.01)")
                        raw_peak, next_beat = 0.0, next_beat + 10.0
                    next_partial = feed_frame(pipe, seg, agc(raw), start,
                                              next_partial)
    except KeyboardInterrupt:
        pass
    if seg.open_segment is not None:
        pipe.submit_final(seg.open_segment.t_start, seg.open_segment.audio, start)
    pipe.idle.wait(timeout=60)
    return pipe


def run_replay(wav_path: str, room: str, play: bool = False,
               stop_event: threading.Event | None = None,
               on_phase=None) -> Pipeline:
    phase = on_phase or (lambda p: None)
    audio, sr = sf.read(wav_path, dtype="float32")
    assert sr == SAMPLE_RATE, f"fixture must be {SAMPLE_RATE} Hz"
    phase("loading models")
    pipe = Pipeline(room)
    pipe.warmup()
    phase("live")
    seg = Segmenter()
    frame_len = int(FRAME_SECONDS * SAMPLE_RATE)
    player = None
    if play:  # audible monitor of the replay, synced to the same clock
        import subprocess
        player = subprocess.Popen(["afplay", wav_path])
    start = time.monotonic()
    next_partial = PARTIAL_INTERVAL_SECONDS
    for i in range(0, len(audio), frame_len):
        if stop_event is not None and stop_event.is_set():
            break
        target = start + i / SAMPLE_RATE
        delay = target - time.monotonic()
        if delay > 0:
            time.sleep(delay)  # pin replay to wall clock = simulate live feed
        next_partial = feed_frame(pipe, seg, audio[i:i + frame_len], start,
                                  next_partial)
    if seg.open_segment is not None:
        pipe.submit_final(seg.open_segment.t_start, seg.open_segment.audio, start)
    pipe.idle.wait(timeout=60)  # drain pending finals before reporting stats
    if player is not None:
        player.terminate()
    return pipe


def main() -> None:
    ap = argparse.ArgumentParser(
        description="Run the M1 pipeline from a WAV replay or the microphone")
    ap.add_argument("wav", nargs="?", help="WAV file to replay (omit with --mic)")
    ap.add_argument("--mic", action="store_true",
                    help="live capture from the default input device")
    ap.add_argument("--duration", type=float, default=None,
                    help="stop mic capture after N seconds (default: Ctrl-C)")
    ap.add_argument("--room", default="room1")
    ap.add_argument("--play", action="store_true",
                    help="also play the audio on the speakers, in sync")
    ap.add_argument("--stats-out", default="data/replay_stats.json")
    args = ap.parse_args()
    if bool(args.wav) == args.mic:
        ap.error("provide either a WAV file or --mic")

    server = uvicorn.Server(uvicorn.Config(
        app, host=SERVER_HOST, port=SERVER_PORT, log_level="warning"))
    threading.Thread(target=server.run, daemon=True).start()
    print(f"Subtitles: http://{SERVER_HOST}:{SERVER_PORT}/subtitles?room={args.room}")
    print("Loading Whisper (warmup)...")
    asr.warmup()
    print("Mic starting." if args.mic else "Replay starting.")
    if args.mic:
        pipe = run_mic(args.room, duration=args.duration)
    else:
        pipe = run_replay(args.wav, args.room, play=args.play)
    if not pipe.lags:
        print("No speech detected; no stats to report.")
        return
    stats = {"segments": pipe.seq, "max_lag_s": round(max(pipe.lags), 2),
             "mean_lag_s": round(sum(pipe.lags) / len(pipe.lags), 2)}
    Path(args.stats_out).write_text(json.dumps(stats, indent=1))
    print(f"Replay done. {stats}")
    deadline = time.monotonic() + 120
    while not pipe.translate_q.empty() and time.monotonic() < deadline:
        time.sleep(1)  # let the translation queue drain and pages update
    time.sleep(10)


if __name__ == "__main__":
    main()
