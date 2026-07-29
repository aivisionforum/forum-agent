"""Neural VAD (Silero): recognizes human speech rather than mere loudness,
so mic input needs no calibration or sensitivity tuning — plug and play."""
import numpy as np
import torch

from forum_agent.constants import (AGC_DECAY, AGC_MAX_GAIN, AGC_TARGET_PEAK,
                                   SILERO_CHUNK, SILERO_MIN_PEAK,
                                   SILERO_SPEECH_PROB)


class AutoGain:
    """Automatic gain control: amplifies quiet mic input (e.g. low system
    input volume) to a healthy level before VAD and ASR. Tracks a slowly
    decaying running peak so gain adapts but does not pump per-frame."""

    def __init__(self) -> None:
        self._peak = 1e-6

    def __call__(self, frame: np.ndarray) -> np.ndarray:
        peak = float(np.max(np.abs(frame))) if len(frame) else 0.0
        self._peak = max(peak, self._peak * AGC_DECAY, 1e-6)
        gain = min(AGC_TARGET_PEAK / self._peak, AGC_MAX_GAIN)
        return frame * gain if gain > 1.0 else frame


class SileroSpeech:
    """Callable frame -> bool for Segmenter.speech_fn."""

    def __init__(self) -> None:
        from silero_vad import load_silero_vad
        self._model = load_silero_vad()

    def __call__(self, frame: np.ndarray) -> bool:
        n = len(frame) // SILERO_CHUNK
        if n == 0:
            return False
        # Fast path: frames far below any speech level skip inference, so the
        # real-time feed thread never stalls on the model during silence.
        if float(np.max(np.abs(frame))) < SILERO_MIN_PEAK:
            return False
        chunks = torch.from_numpy(
            frame[:n * SILERO_CHUNK].reshape(n, SILERO_CHUNK).copy())
        with torch.no_grad():  # one batched call: n python/GIL round-trips -> 1
            probs = self._model(chunks, 16000)
        return float(probs.max()) >= SILERO_SPEECH_PROB
