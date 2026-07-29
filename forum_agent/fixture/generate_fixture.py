"""Generate the ~10-minute synthetic bilingual meeting fixture via macOS `say`.

Usage: python -m forum_agent.fixture.generate_fixture
Writes data/fixture_meeting.wav (16 kHz mono) and data/fixture_reference.json
(ground-truth speaker/text/timing for eyeballing diarization quality).
"""
import json
import subprocess
import sys
import tempfile
from pathlib import Path

import numpy as np
import soundfile as sf

from forum_agent.constants import FIXTURE_REF_JSON, FIXTURE_WAV, SAMPLE_RATE
from forum_agent.fixture.script import LINES

VOICES = {"A": "Tingting", "B": "Meijia", "C": "Samantha", "D": "Daniel"}
GAP_SECONDS = 0.9
SAY_RATE = {"A": 160, "B": 160, "C": 175, "D": 175}


def synthesize_line(voice: str, rate: int, text: str, out_wav: Path) -> np.ndarray:
    with tempfile.NamedTemporaryFile(suffix=".aiff", delete=False) as tmp:
        aiff = Path(tmp.name)
    subprocess.run(["say", "-v", voice, "-r", str(rate), "-o", str(aiff), text],
                   check=True)
    subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-i", str(aiff),
                    "-ar", str(SAMPLE_RATE), "-ac", "1", str(out_wav)], check=True)
    aiff.unlink()
    audio, sr = sf.read(out_wav)
    assert sr == SAMPLE_RATE
    return audio.astype(np.float32)


def main() -> None:
    chunks, reference = [], []
    cursor = 0.0
    gap = np.zeros(int(GAP_SECONDS * SAMPLE_RATE), dtype=np.float32)
    with tempfile.TemporaryDirectory() as tdir:
        for i, (speaker, text) in enumerate(LINES):
            wav = Path(tdir) / f"line_{i:03d}.wav"
            audio = synthesize_line(VOICES[speaker], SAY_RATE[speaker], text, wav)
            dur = len(audio) / SAMPLE_RATE
            reference.append({"speaker": speaker, "voice": VOICES[speaker],
                              "t_start": round(cursor, 2),
                              "t_end": round(cursor + dur, 2), "text": text})
            chunks.extend([audio, gap])
            cursor += dur + GAP_SECONDS
            print(f"[{i + 1}/{len(LINES)}] {speaker} {dur:5.1f}s  total {cursor:6.1f}s",
                  file=sys.stderr)
    full = np.concatenate(chunks)
    Path(FIXTURE_WAV).parent.mkdir(exist_ok=True)
    sf.write(FIXTURE_WAV, full, SAMPLE_RATE)
    Path(FIXTURE_REF_JSON).write_text(
        json.dumps(reference, ensure_ascii=False, indent=1))
    print(f"Wrote {FIXTURE_WAV}: {cursor / 60:.1f} min, {len(LINES)} utterances")


if __name__ == "__main__":
    main()
