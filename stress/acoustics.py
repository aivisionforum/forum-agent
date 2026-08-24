"""Synthetic room acoustics for stress fixtures.

No venue recording is available, so conference-room conditions are modelled:
HVAC/fan noise, audience babble, reverberation, and far-field level loss.
This is a proxy, not the room -- absolute thresholds still need a real
recording. What it does give is a repeatable sweep: the same speech under
monotonically harder conditions, so we can see WHERE the pipeline breaks
even if the exact SNR at which it breaks differs at the venue.
"""
import numpy as np
from scipy.signal import fftconvolve, lfilter

SR = 16000
_rng = np.random.default_rng(20260824)


def pink(n: int) -> np.ndarray:
    """1/f noise via a one-pole cascade (Voss-McCartney approximation)."""
    white = _rng.standard_normal(n)
    # 3 dB/octave rolloff filter coefficients (Paul Kellet's economy design)
    b = [0.049922035, -0.095993537, 0.050612699, -0.004408786]
    a = [1.0, -2.494956002, 2.017265875, -0.522189400]
    out = lfilter(b, a, white)
    return (out / (np.max(np.abs(out)) + 1e-12)).astype(np.float32)


def hvac(n: int, sr: int = SR) -> np.ndarray:
    """Building ventilation: low-frequency broadband plus mains hum and a
    slow swell as the compressor loads and unloads."""
    t = np.arange(n) / sr
    base = pink(n)
    # low-pass the pink noise twice: HVAC energy is mostly below ~500 Hz
    lp = lfilter([0.06], [1.0, -0.94], base)
    lp = lfilter([0.06], [1.0, -0.94], lp)
    hum = (0.25 * np.sin(2 * np.pi * 50 * t)
           + 0.12 * np.sin(2 * np.pi * 100 * t)
           + 0.05 * np.sin(2 * np.pi * 150 * t))
    swell = 1.0 + 0.25 * np.sin(2 * np.pi * t / 37.0)
    out = (lp / (np.std(lp) + 1e-12) * 0.8 + hum) * swell
    return (out / (np.max(np.abs(out)) + 1e-12)).astype(np.float32)


def babble(n: int, bank: list, sr: int = SR, voices: int = 12) -> np.ndarray:
    """Audience murmur: many speech clips overlapped at random offsets, which
    is far harder for a neural VAD than stationary noise -- it IS speech,
    just not the speech we want."""
    out = np.zeros(n, dtype=np.float32)
    for _ in range(voices):
        pos = 0
        while pos < n:
            clip = bank[_rng.integers(len(bank))]
            seg = clip[: min(len(clip), n - pos)]
            out[pos:pos + len(seg)] += seg * _rng.uniform(0.5, 1.0)
            pos += len(seg) + int(_rng.uniform(0.1, 1.2) * sr)
    return (out / (np.max(np.abs(out)) + 1e-12)).astype(np.float32)


def rir(rt60: float, sr: int = SR, n_early: int = 12) -> np.ndarray:
    """Synthetic room impulse response.

    The direct path stays at sample 0 with unit gain, so convolution does not
    shift onsets -- ground-truth timings from the dry fixture remain valid.
    Sparse early reflections (a hard table, a projector screen) sit in the
    first 60 ms; the late tail is exponentially decaying diffuse noise.
    """
    n = int(rt60 * 1.5 * sr)
    h = np.zeros(n, dtype=np.float32)
    h[0] = 1.0
    for _ in range(n_early):
        idx = int(_rng.uniform(0.004, 0.06) * sr)
        h[idx] += _rng.uniform(-0.5, 0.5)
    t = np.arange(n) / sr
    tail = _rng.standard_normal(n) * np.exp(-6.9078 * t / rt60)
    tail[: int(0.008 * sr)] = 0.0
    h += (tail * 0.35).astype(np.float32)
    return h


def reverberate(x: np.ndarray, rt60: float, sr: int = SR) -> np.ndarray:
    """Convolve with a synthetic RIR, preserving length and peak level."""
    wet = fftconvolve(x, rir(rt60, sr))[: len(x)]
    peak = np.max(np.abs(wet)) + 1e-12
    return (wet / peak * (np.max(np.abs(x)) + 1e-12)).astype(np.float32)


def speech_rms(x: np.ndarray, spans: list, sr: int = SR) -> float:
    """RMS over reference speech spans only -- measuring over the whole file
    would count the gaps and overstate SNR."""
    parts = [x[int(a * sr):int(b * sr)] for a, b in spans]
    parts = [p for p in parts if len(p)]
    if not parts:
        return float(np.sqrt(np.mean(x ** 2)) + 1e-12)
    cat = np.concatenate(parts)
    return float(np.sqrt(np.mean(cat ** 2)) + 1e-12)


def mix_at_snr(speech: np.ndarray, noise: np.ndarray, snr_db: float,
               spans: list, sr: int = SR) -> np.ndarray:
    """Add noise scaled so speech-to-noise ratio hits snr_db."""
    s_rms = speech_rms(speech, spans, sr)
    n_rms = float(np.sqrt(np.mean(noise ** 2)) + 1e-12)
    target_n = s_rms / (10 ** (snr_db / 20.0))
    out = speech + noise * (target_n / n_rms)
    peak = np.max(np.abs(out))
    if peak > 0.99:  # keep headroom rather than clip
        out = out / peak * 0.99
    return out.astype(np.float32)


def attenuate(x: np.ndarray, db: float) -> np.ndarray:
    """Level loss from mic distance. AGC is supposed to recover this."""
    return (x * 10 ** (-abs(db) / 20.0)).astype(np.float32)


def measured_snr(mixed: np.ndarray, spans: list, sr: int = SR) -> float:
    """Verify a fixture after the fact: speech-span RMS vs gap RMS."""
    n = len(mixed)
    mask = np.zeros(n, dtype=bool)
    for a, b in spans:
        mask[int(a * sr):min(int(b * sr), n)] = True
    sp = mixed[mask]
    gap = mixed[~mask]
    if not len(sp) or not len(gap):
        return float("nan")
    s = np.sqrt(np.mean(sp ** 2)) + 1e-12
    g = np.sqrt(np.mean(gap ** 2)) + 1e-12
    return float(20 * np.log10(s / g))
