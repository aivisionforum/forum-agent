"""Build hostile replay fixtures from the existing synthetic meeting.

Per-utterance audio is sliced out of data/fixture_meeting.wav using the
timings in data/fixture_reference.json, so every condition below is composed
from the SAME speech -- differences in the results are caused by the
condition, not by different words. Only the 15-speaker condition needs new
`say` synthesis.

    .venv/bin/python -m stress.build              # all conditions
    .venv/bin/python -m stress.build --only noise12 reverb

Each condition writes data/stress/<name>.wav plus <name>_ref.json in the
same schema as fixture_reference.json (speaker, voice, t_start, t_end, text),
which stress/metrics.py scores against.
"""
import argparse
import json
import re
from pathlib import Path

import numpy as np
import soundfile as sf

from stress import acoustics as ac

SR = ac.SR
OUT = Path("data/stress")
BASE_SECONDS = 180.0          # per-condition replay length (wall-clock cost)
SILENCE_SECONDS = 600.0       # empty-room soak
_CJK = re.compile(r"[一-鿿]")


def _load_bank() -> tuple[np.ndarray, list, list]:
    """Returns (full audio, reference list, per-utterance clips)."""
    audio, sr = sf.read("data/fixture_meeting.wav", dtype="float32")
    assert sr == SR, f"fixture must be {SR} Hz"
    ref = json.loads(Path("data/fixture_reference.json").read_text())
    clips = [audio[int(u["t_start"] * SR):int(u["t_end"] * SR)] for u in ref]
    return audio, ref, clips


def _spans(ref: list) -> list:
    return [(u["t_start"], u["t_end"]) for u in ref]


def _write(name: str, audio: np.ndarray, ref: list, note: str) -> dict:
    OUT.mkdir(parents=True, exist_ok=True)
    sf.write(OUT / f"{name}.wav", audio, SR)
    (OUT / f"{name}_ref.json").write_text(
        json.dumps(ref, ensure_ascii=False, indent=1))
    snr = ac.measured_snr(audio, _spans(ref)) if ref else float("nan")
    meta = {"name": name, "seconds": round(len(audio) / SR, 1),
            "utterances": len(ref),
            "speakers": len({u["speaker"] for u in ref}),
            "measured_snr_db": None if np.isnan(snr) else round(snr, 1),
            "note": note}
    print(f"  {name:<12} {meta['seconds']:6.1f}s  {meta['utterances']:3d} utt  "
          f"{meta['speakers']:2d} spk  snr={meta['measured_snr_db']}  {note}")
    return meta


def base_excerpt(ref: list, audio: np.ndarray) -> tuple[np.ndarray, list]:
    """First BASE_SECONDS of the fixture, timings untouched.

    Cut at the end of the last WHOLE utterance, not at the second mark: a
    clipped final utterance would be speech with no ground truth, and the
    metrics would score it as hallucinated text.
    """
    keep = [u for u in ref if u["t_end"] <= BASE_SECONDS]
    end = keep[-1]["t_end"] + 0.45  # half the inter-utterance gap
    return audio[: int(end * SR)].copy(), keep


def compose(clips: list, ref: list, order: list, gap: float,
            overlap_every: int = 0, overlap_by: float = 0.0
            ) -> tuple[np.ndarray, list]:
    """Lay the given utterance indices end to end. Every `overlap_every`-th
    turn starts `overlap_by` seconds before the previous one ends."""
    out, new_ref, cursor = [], [], 0.0
    for n, i in enumerate(order):
        clip = clips[i]
        dur = len(clip) / SR
        start = cursor
        if overlap_every and n and n % overlap_every == 0:
            start = max(0.0, cursor - overlap_by)
        new_ref.append({**ref[i], "t_start": round(start, 3),
                        "t_end": round(start + dur, 3)})
        out.append((start, clip))
        cursor = max(cursor, start + dur) + gap
    total = int((cursor + 1.0) * SR)
    buf = np.zeros(total, dtype=np.float32)
    for start, clip in out:
        s = int(start * SR)
        buf[s:s + len(clip)] += clip
    peak = np.max(np.abs(buf))
    if peak > 0.99:
        buf = buf / peak * 0.99
    return buf.astype(np.float32), new_ref


def build_voices15(n_speakers: int = 15, seconds: float = BASE_SECONDS) -> tuple:
    """Fresh synthesis with 15 distinct voices -- the only condition that
    needs `say`. zh lines go to zh voices, en lines to en voices, so the
    audio stays plausible. Exercises MAX_SPEAKERS=8 in diarize.py."""
    from forum_agent.fixture.generate_fixture import synthesize_line
    from forum_agent.fixture.script import LINES
    import tempfile
    zh_voices = ["Tingting", "Meijia", "Sinji",
                 "Eddy (Chinese (China mainland))",
                 "Flo (Chinese (China mainland))",
                 "Sandy (Chinese (China mainland))",
                 "Shelley (Chinese (China mainland))",
                 "Grandpa (Chinese (China mainland))"]
    en_voices = ["Samantha", "Daniel", "Karen", "Moira", "Tessa", "Aman",
                 "Rishi"]
    zh_voices = zh_voices[: max(1, round(n_speakers * 8 / 15))]
    en_voices = en_voices[: n_speakers - len(zh_voices)]
    slots = ([(f"S{i:02d}", v, "zh") for i, v in enumerate(zh_voices)]
             + [(f"S{i:02d}", v, "en")
                for i, v in enumerate(en_voices, len(zh_voices))])
    cache = OUT / "lines15"
    cache.mkdir(parents=True, exist_ok=True)
    chunks, ref, cursor = [], [], 0.0
    gap = np.zeros(int(0.9 * SR), dtype=np.float32)
    zh_i = en_i = 0
    for _, text in LINES:
        lang = "zh" if _CJK.search(text) else "en"
        pool = [s for s in slots if s[2] == lang]
        if not pool:
            continue
        if lang == "zh":
            slot = pool[zh_i % len(pool)]; zh_i += 1
        else:
            slot = pool[en_i % len(pool)]; en_i += 1
        key = cache / f"{abs(hash((slot[1], text))):016x}.wav"
        if not key.exists():
            with tempfile.TemporaryDirectory() as td:
                audio = synthesize_line(slot[1], 170, text, Path(td) / "l.wav")
            sf.write(key, audio, SR)
        clip, _ = sf.read(key, dtype="float32")
        dur = len(clip) / SR
        ref.append({"speaker": slot[0], "voice": slot[1],
                    "t_start": round(cursor, 3), "t_end": round(cursor + dur, 3),
                    "text": text})
        chunks.extend([clip, gap])
        cursor += dur + 0.9
        if cursor >= seconds:
            break
    return np.concatenate(chunks).astype(np.float32), ref


def build_monologue(clips: list, ref: list, seconds: float = BASE_SECONDS):
    """One speaker, only 0.25 s pauses: shorter than VAD_SILENCE_SECONDS
    (0.5 s), so turns can only be closed by MAX_SEGMENT_SECONDS=12."""
    idx = [i for i, u in enumerate(ref) if u["speaker"] == ref[0]["speaker"]]
    order, total = [], 0.0
    while total < seconds and idx:
        for i in idx:
            order.append(i)
            total += (ref[i]["t_end"] - ref[i]["t_start"]) + 0.25
            if total >= seconds:
                break
    return compose(clips, ref, order, gap=0.25)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--only", nargs="*", default=None)
    args = ap.parse_args()
    audio, ref, clips = _load_bank()
    base, base_ref = base_excerpt(ref, audio)
    spans = _spans(base_ref)
    want = (lambda n: args.only is None or n in args.only)
    manifest = []
    print(f"base excerpt: {len(base)/SR:.1f}s, {len(base_ref)} utterances")

    if want("clean"):
        manifest.append(_write("clean", base, base_ref,
                               "control: dry close-mic TTS"))
    if want("silence"):
        n = int(SILENCE_SECONDS * SR)
        level = ac.speech_rms(base, spans) / (10 ** (12 / 20.0))
        room = ac.hvac(n)
        room = room / (np.sqrt(np.mean(room ** 2)) + 1e-12) * level
        manifest.append(_write("silence", room.astype(np.float32), [],
                               "empty room tone, no speech at all"))
    for snr in (20, 12, 6):
        name = f"noise{snr:02d}"
        if want(name):
            mixed = ac.mix_at_snr(base, ac.hvac(len(base)), snr, spans)
            manifest.append(_write(name, mixed, base_ref,
                                   f"HVAC noise at {snr} dB SNR"))
    if want("reverb"):
        manifest.append(_write("reverb", ac.reverberate(base, 0.6), base_ref,
                               "RT60 0.6 s, dry level otherwise"))
    if want("farfield"):
        wet = ac.reverberate(base, 0.8)
        wet = ac.attenuate(wet, 18)
        mixed = ac.mix_at_snr(wet, ac.hvac(len(wet)), 12, spans)
        manifest.append(_write("farfield", mixed, base_ref,
                               "RT60 0.8 s, -18 dB level, 12 dB SNR"))
    if want("babble"):
        bed = ac.babble(len(base), clips)
        manifest.append(_write("babble", ac.mix_at_snr(base, bed, 10, spans),
                               base_ref, "audience murmur bed at 10 dB SNR"))
    if want("overlap"):
        order = [i for i, u in enumerate(ref) if u["t_end"] <= BASE_SECONDS]
        buf, oref = compose(clips, ref, order, gap=0.9,
                            overlap_every=3, overlap_by=1.8)
        manifest.append(_write("overlap", buf, oref,
                               "every 3rd turn starts 1.8 s early"))
    if want("monologue"):
        buf, mref = build_monologue(clips, ref)
        manifest.append(_write("monologue", buf, mref,
                               "single speaker, 0.25 s pauses only"))
    if want("voices15"):
        buf, vref = build_voices15()
        manifest.append(_write("voices15", buf, vref,
                               "15 distinct voices vs MAX_SPEAKERS=8"))

    mpath = OUT / "manifest.json"
    old = {m["name"]: m for m in
           (json.loads(mpath.read_text()) if mpath.exists() else [])}
    for m in manifest:
        old[m["name"]] = m
    mpath.write_text(json.dumps(list(old.values()), ensure_ascii=False, indent=1))
    print(f"wrote {len(manifest)} condition(s) to {OUT}")


if __name__ == "__main__":
    main()
