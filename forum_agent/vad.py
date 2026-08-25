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
        # Silero is a streaming RNN: it carries hidden state between calls and
        # expects ONE 512-sample chunk at a time. Passing n chunks as a batch
        # makes it treat them as n parallel streams sharing one state, and the
        # state then latches: after speech excites it, a noise floor keeps it
        # fed and every later frame reads as speech, so the segmenter never
        # sees VAD_SILENCE_SECONDS and every turn force-closes at
        # MAX_SEGMENT_SECONDS. Measured on HVAC noise at 12 dB SNR: 100% of
        # inter-turn gaps classified as speech batched, 4.7% sequential.
        # Every chunk is fed even once the frame is known to be speech:
        # skipping one would leave the state a chunk behind the audio.
        buf = frame[:n * SILERO_CHUNK].reshape(n, SILERO_CHUNK)
        best = 0.0
        with torch.no_grad():
            for chunk in buf:
                prob = float(self._model(
                    torch.from_numpy(chunk.copy()).unsqueeze(0), 16000))
                best = max(best, prob)
        return best >= SILERO_SPEECH_PROB
