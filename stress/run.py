"""Replay every stress condition through the real pipeline and score it.

    .venv/bin/python -m stress.run                  # all, skipping done ones
    .venv/bin/python -m stress.run --only noise06   # one condition
    .venv/bin/python -m stress.run --redo           # ignore existing results

Runs in-process (no uvicorn): forum_agent.server.hub returns early when no
event loop is registered, which is the module's documented standalone mode.
The mlx-lm server is launched once for the whole matrix. Each condition gets
its own room name so transcripts never mix, and results are appended to
data/stress/results.json as they land -- the matrix costs real time (replay
is wall-clock pinned), so partial results have to be usable.
"""
import argparse
import json
import subprocess
import threading
import time
from pathlib import Path

from forum_agent import llm, replay
from stress.metrics import score

FRONTS = ("energy", "neural")

RESULTS = Path("data/stress/results.json")
ORDER = ["clean", "noise20", "noise12", "noise06", "reverb", "farfield",
         "babble", "overlap", "monologue", "voices15", "silence"]


def _key(cond: str, front: str) -> str:
    return cond if front == "energy" else f"{cond}:{front}"


def _load() -> dict:
    rows = json.loads(RESULTS.read_text()) if RESULTS.exists() else []
    return {_key(r["condition"], r.get("front", "energy")): r for r in rows}


def _save(rows: dict) -> None:
    keys = [_key(c, f) for f in FRONTS for c in ORDER]
    RESULTS.write_text(json.dumps(
        [rows[k] for k in keys if k in rows], ensure_ascii=False, indent=1))


def _footprint() -> tuple[float, float]:
    """(resident GB across pipeline processes, swap used GB). This box is a
    16 GB M3, not the 128 GB M3 Max the project benchmarked on, so memory
    pressure is a first-class result rather than a footnote."""
    rss = 0.0
    try:
        out = subprocess.run(["ps", "-axo", "rss=,command="],
                             capture_output=True, text=True, timeout=10).stdout
        for line in out.splitlines():
            parts = line.strip().split(None, 1)
            if len(parts) == 2 and ("mlx_lm" in parts[1]
                                    or "stress.run" in parts[1]):
                rss += int(parts[0]) / 1024 / 1024
    except Exception:
        pass
    swap = 0.0
    try:
        out = subprocess.run(["sysctl", "-n", "vm.swapusage"],
                             capture_output=True, text=True, timeout=10).stdout
        for tok in out.split():
            if tok.endswith("M") and "used" in out.split(tok)[0][-8:]:
                swap = float(tok[:-1]) / 1024
                break
    except Exception:
        pass
    return rss, swap


class Sampler(threading.Thread):
    """Peak memory footprint while one condition replays."""

    def __init__(self) -> None:
        super().__init__(daemon=True)
        self.stop = threading.Event()
        self.peak_rss = self.peak_swap = 0.0

    def run(self) -> None:
        while not self.stop.wait(5.0):
            rss, swap = _footprint()
            self.peak_rss = max(self.peak_rss, rss)
            self.peak_swap = max(self.peak_swap, swap)


def replay_neural(wav: str, room: str):
    """Replay a file through the MIC front end: auto-gain plus the Silero
    neural VAD.

    forum_agent.replay.run_replay uses Segmenter() with no speech_fn, i.e.
    the energy VAD at VAD_ENERGY_THRESHOLD = 1e-4 mean-square -- so any room
    tone above about -40 dBFS is permanent speech and turns only ever close
    at MAX_SEGMENT_SECONDS. Silero is wired into run_mic only. At the venue
    the input is a mic, so the neural path is the one that matters; this
    reproduces it from a file, which is the only way to test it repeatably
    without a microphone.
    """
    import time as _t

    import soundfile as sf

    from forum_agent.constants import (FRAME_SECONDS, PARTIAL_INTERVAL_SECONDS,
                                       SAMPLE_RATE)
    from forum_agent.pipeline import Pipeline
    from forum_agent.replay import _drain_and_close, feed_frame
    from forum_agent.segmenter import Segmenter
    from forum_agent.vad import AutoGain, SileroSpeech

    audio, sr = sf.read(wav, dtype="float32")
    assert sr == SAMPLE_RATE, f"fixture must be {SAMPLE_RATE} Hz"
    pipe = Pipeline(room)
    pipe.warmup()
    seg = Segmenter(speech_fn=SileroSpeech())
    agc = AutoGain()
    frame_len = int(FRAME_SECONDS * SAMPLE_RATE)
    start = _t.monotonic()
    next_partial = PARTIAL_INTERVAL_SECONDS
    for i in range(0, len(audio), frame_len):
        delay = start + i / SAMPLE_RATE - _t.monotonic()
        if delay > 0:
            _t.sleep(delay)  # pin to wall clock, exactly as run_replay does
        next_partial = feed_frame(pipe, seg, agc(audio[i:i + frame_len]),
                                  start, next_partial)
    if seg.open_segment is not None:
        pipe.submit_final(seg.open_segment.t_start, seg.open_segment.audio,
                          start)
    _drain_and_close(pipe)
    return pipe


def run_one(cond: str, front: str = "energy") -> dict:
    wav = Path(f"data/stress/{cond}.wav")
    if not wav.exists():
        raise FileNotFoundError(f"{wav} -- run stress.build first")
    sampler = Sampler()
    sampler.start()
    room = cond if front == "energy" else f"{cond}-n"
    t0 = time.time()
    try:
        pipe = (replay.run_replay(str(wav), room, play=False)
                if front == "energy" else replay_neural(str(wav), room))
    finally:
        sampler.stop.set()
    wall = time.time() - t0
    row = score(cond, room=room)
    row["front"] = front
    row["wall_seconds"] = round(wall, 1)
    row["max_lag_s"] = round(max(pipe.lags), 2) if pipe.lags else None
    row["mean_lag_s"] = round(sum(pipe.lags) / len(pipe.lags), 2) \
        if pipe.lags else None
    row["peak_rss_gb"] = round(sampler.peak_rss, 1)
    row["peak_swap_gb"] = round(sampler.peak_swap, 1)
    return row


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--only", nargs="*", default=None)
    ap.add_argument("--redo", action="store_true")
    ap.add_argument("--front", choices=FRONTS, default="energy",
                    help="energy = run_replay as shipped; neural = the mic "
                         "front end (auto-gain + Silero) fed from the file")
    args = ap.parse_args()
    todo = args.only or ORDER
    rows = _load()  # --redo re-runs the requested conditions only; it must
    # never discard results for conditions it was not asked to touch
    proc = llm.launch_server()
    if proc is not None:
        print("[matrix] mlx-lm server launched", flush=True)
    llm.prewarm()
    for cond in todo:
        key = _key(cond, args.front)
        if key in rows and not args.redo:
            print(f"[matrix] {key}: cached, skipping", flush=True)
            continue
        print(f"[matrix] {key}: replaying...", flush=True)
        try:
            rows[key] = run_one(cond, args.front)
        except Exception as exc:  # one bad condition must not kill the matrix
            rows[key] = {"condition": cond, "front": args.front,
                         "error": f"{type(exc).__name__}: {exc}"}
            print(f"[matrix] {key}: FAILED {exc!r}", flush=True)
        _save(rows)
        print(f"[matrix] {key}: {json.dumps(rows[key], ensure_ascii=False)}",
              flush=True)
    print("[matrix] done", flush=True)


if __name__ == "__main__":
    main()
