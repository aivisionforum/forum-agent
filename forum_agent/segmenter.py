"""Energy-based VAD segmenter: turns a live audio stream into utterances."""
from dataclasses import dataclass, field

import numpy as np

from forum_agent.constants import (MAX_SEGMENT_SECONDS, PRE_ROLL_SECONDS,
                                   SAMPLE_RATE, VAD_ENERGY_THRESHOLD,
                                   VAD_SILENCE_SECONDS)


@dataclass
class Segment:
    t_start: float
    audio: np.ndarray
    closed: bool = False

    @property
    def duration(self) -> float:
        return len(self.audio) / SAMPLE_RATE


@dataclass
class Segmenter:
    """Feed audio frames; yields open (partial) and closed segments."""
    energy_threshold: float = VAD_ENERGY_THRESHOLD  # used when speech_fn unset
    silence_seconds: float = VAD_SILENCE_SECONDS    # gap that closes a turn
    speech_fn: object = None  # optional callable frame->bool (neural VAD)
    _current: Segment | None = None
    _silence: float = 0.0
    _clock: float = 0.0
    _preroll: list = field(default_factory=list)

    def feed(self, frame: np.ndarray) -> list[Segment]:
        """Feed one frame; returns segments closed by this frame."""
        frame_dur = len(frame) / SAMPLE_RATE
        if self.speech_fn is not None:
            is_speech = bool(self.speech_fn(frame))
        else:
            is_speech = float(np.mean(frame ** 2)) > self.energy_threshold
        closed: list[Segment] = []
        if self._current is None:
            if is_speech:
                # prepend pre-roll so word onsets before VAD triggered survive
                audio = np.concatenate(self._preroll + [frame])
                t0 = self._clock - sum(len(f) for f in self._preroll) / SAMPLE_RATE
                self._current = Segment(t_start=max(t0, 0.0), audio=audio)
                self._silence = 0.0
                self._preroll = []
            else:
                self._preroll.append(frame)
                keep = int(PRE_ROLL_SECONDS / frame_dur) if frame_dur else 1
                self._preroll = self._preroll[-max(keep, 1):]
        else:
            self._current.audio = np.concatenate([self._current.audio, frame])
            self._silence = 0.0 if is_speech else self._silence + frame_dur
            if (self._silence >= self.silence_seconds
                    or self._current.duration >= MAX_SEGMENT_SECONDS):
                self._current.closed = True
                closed.append(self._current)
                self._current = None
        self._clock += frame_dur
        return closed

    @property
    def clock(self) -> float:
        """Stream position in seconds (total audio fed so far)."""
        return self._clock

    @property
    def open_segment(self) -> Segment | None:
        return self._current
