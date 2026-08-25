"""Replay/mic orchestrators: one-command startup.

    python -m forum_agent.replay data/fixture_meeting.wav --room room1

Starts the subtitle server, then replays the WAV pinned to wall-clock time
(simulating a live feed), running VAD -> Whisper -> diarization -> JSONL ->
websocket, with translation on a separate worker so subtitles never wait.
"""
import argparse
import json
import threading
import time
from pathlib import Path

import soundfile as sf
import uvicorn

from forum_agent import asr
from forum_agent.constants import (DEAD_STREAM_SECONDS, FRAME_SECONDS,
                                   PARTIAL_INTERVAL_SECONDS, RECORDING_WAV,
                                   SAMPLE_RATE, SERVER_HOST, SERVER_PORT)
from forum_agent.pipeline import Pipeline
from forum_agent.segmenter import Segmenter
from forum_agent.server import app
from forum_agent.vad import AutoGain, SileroSpeech




def feed_frame(pipe: Pipeline, seg: Segmenter, frame, start: float,
               next_partial: float) -> float:
    """Push one frame through VAD; submit finals/partials. Returns updated
    next-partial deadline."""
    for closed in seg.feed(frame):
        pipe.submit_final(closed.t_start, closed.audio, start)
        next_partial = seg.clock + PARTIAL_INTERVAL_SECONDS
    cur = seg.open_segment
    if cur is not None and seg.clock >= next_partial:
        pipe.submit_partial(cur.t_start, cur.audio.copy(), start)
        next_partial = seg.clock + PARTIAL_INTERVAL_SECONDS
    return next_partial


def run_mic(room: str, duration: float | None = None,
            stop_event: threading.Event | None = None,
            on_phase=None, device: int | None = None,
            on_level=None, record: bool = True) -> Pipeline:
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
    # C3 raw audio backup — on by default, disable per session (docs/DATA_HANDLING.md)
    recorder = sf.SoundFile(RECORDING_WAV.format(room=room), "w",
                            samplerate=SAMPLE_RATE, channels=1) \
        if record else None
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
                    if recorder is not None:
                        recorder.write(raw)
                    if overflowed:
                        print("[mic] input overflow: audio dropped by OS")
                    peak = float(np.max(np.abs(raw)))
                    if on_level is not None:  # console level meter (issue #7)
                        on_level(peak, float(np.sqrt(np.mean(raw ** 2))))
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
    finally:
        if recorder is not None:
            recorder.close()
    if seg.open_segment is not None:
        pipe.submit_final(seg.open_segment.t_start, seg.open_segment.audio, start)
    _drain_and_close(pipe)
    return pipe


def _drain_and_close(pipe: Pipeline, budget: float | None = None) -> None:
    """Let pending ASR finals and translations finish, then stop workers.

    The default budget suits a live feed: wall-clock pinning keeps ASR in
    step with the input, so at end-of-feed only an utterance or two is
    queued. Batch processing removes that pacing -- the feed can push an
    hour of audio through the VAD in a couple of minutes, leaving the whole
    file in the queues -- and close() discards whatever is still pending,
    silently truncating transcript and translations alike. Callers that feed
    faster than real time must pass a budget scaled to the audio.
    """
    idle_budget = 60.0 if budget is None else budget
    pipe.idle.wait(timeout=idle_budget)
    deadline = time.monotonic() + (90.0 if budget is None else budget)
    while not pipe.translate_q.empty() and time.monotonic() < deadline:
        time.sleep(0.5)
    pipe.close()


def run_replay(wav_path: str, room: str, play: bool = False,
               stop_event: threading.Event | None = None,
               on_phase=None, realtime: bool = True) -> Pipeline:
    """realtime=True pins frames to wall clock (simulates a live feed);
    realtime=False feeds as fast as inference allows — batch processing of
    an uploaded recording (issue #8)."""
    phase = on_phase or (lambda p: None)
    audio, sr = sf.read(wav_path, dtype="float32")
    assert sr == SAMPLE_RATE, f"fixture must be {SAMPLE_RATE} Hz"
    phase("loading models")
    pipe = Pipeline(room)
    pipe.warmup()
    phase("live")
    # Same front end as run_mic: auto-gain into the Silero neural VAD. With
    # the default energy VAD, replay gates on absolute level
    # (VAD_ENERGY_THRESHOLD = 1e-4 mean-square = -40 dBFS), so any recording
    # whose room tone sits above that is one unbroken utterance and turns
    # close only at MAX_SEGMENT_SECONDS. That also made the replay path
    # behave differently from the live path it is meant to rehearse.
    seg = Segmenter(speech_fn=SileroSpeech())
    agc = AutoGain()
    frame_len = int(FRAME_SECONDS * SAMPLE_RATE)
    player = None
    if play and realtime:  # audible monitor of the replay, synced to the clock
        import subprocess
        player = subprocess.Popen(["afplay", wav_path])
    start = time.monotonic()
    next_partial = PARTIAL_INTERVAL_SECONDS
    for i in range(0, len(audio), frame_len):
        if stop_event is not None and stop_event.is_set():
            break
        if realtime:
            delay = start + i / SAMPLE_RATE - time.monotonic()
            if delay > 0:
                time.sleep(delay)  # pin to wall clock = simulate live feed
        next_partial = feed_frame(pipe, seg, agc(audio[i:i + frame_len]),
                                  start, next_partial)
    if seg.open_segment is not None:
        pipe.submit_final(seg.open_segment.t_start, seg.open_segment.audio, start)
    if player is not None:
        player.terminate()
    # Batch runs finish the queued work however long it takes (bounded well
    # above the ~0.3x real time the pipeline needs), rather than truncating.
    _drain_and_close(pipe, budget=None if realtime
                     else max(300.0, 3.0 * len(audio) / SAMPLE_RATE))
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
    ap.add_argument("--no-record", action="store_true",
                    help="do not save raw mic audio to WAV (saved by "
                         "default as backup; see docs/DATA_HANDLING.md)")
    ap.add_argument("--play", action="store_true",
                    help="also play the audio on the speakers, in sync")
    ap.add_argument("--stats-out", default="data/replay_stats.json")
    args = ap.parse_args()
    if bool(args.wav) == args.mic:
        ap.error("provide either a WAV file or --mic")

    import atexit
    from forum_agent import llm
    proc = llm.launch_server()  # managed MLX model server (one command)
    if proc is not None:
        atexit.register(proc.terminate)
    server = uvicorn.Server(uvicorn.Config(
        app, host=SERVER_HOST, port=SERVER_PORT, log_level="warning"))
    threading.Thread(target=server.run, daemon=True).start()
    print(f"Subtitles: http://{SERVER_HOST}:{SERVER_PORT}/subtitles?room={args.room}")
    print("Loading Whisper (warmup)...")
    asr.warmup()
    print("Mic starting." if args.mic else "Replay starting.")
    if args.mic:
        pipe = run_mic(args.room, duration=args.duration, record=not args.no_record)
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
