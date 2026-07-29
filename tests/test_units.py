"""Unit tests: segmenter and language detection. Run: pytest tests/"""
import numpy as np

from forum_agent.asr import detect_lang
from forum_agent.constants import (LANG_EN, LANG_MIXED, LANG_ZH,
                                   MAX_SEGMENT_SECONDS, SAMPLE_RATE)
from forum_agent.segmenter import Segmenter


def _frames(signal, frame_s=0.5):
    n = int(frame_s * SAMPLE_RATE)
    return [signal[i:i + n] for i in range(0, len(signal), n)]


def _speech(seconds):
    rng = np.random.default_rng(0)
    return (rng.normal(0, 0.1, int(seconds * SAMPLE_RATE))).astype(np.float32)


def _silence(seconds):
    return np.zeros(int(seconds * SAMPLE_RATE), dtype=np.float32)


def test_segment_closes_on_silence():
    seg = Segmenter()
    closed = []
    stream = np.concatenate([_speech(3), _silence(1.5), _speech(2), _silence(1.5)])
    for f in _frames(stream):
        closed += seg.feed(f)
    assert len(closed) == 2
    assert abs(closed[0].t_start - 0.0) < 0.6
    assert 3.0 <= closed[0].duration <= 4.0  # speech + trailing silence
    assert abs(closed[1].t_start - 4.5) < 0.6


def test_long_segment_force_closes():
    seg = Segmenter()
    closed = []
    for f in _frames(_speech(MAX_SEGMENT_SECONDS + 5)):
        closed += seg.feed(f)
    assert closed and closed[0].duration <= MAX_SEGMENT_SECONDS + 0.5


def test_silence_only_yields_nothing():
    seg = Segmenter()
    closed = []
    for f in _frames(_silence(5)):
        closed += seg.feed(f)
    assert not closed and seg.open_segment is None


def test_detect_lang():
    assert detect_lang("我们需要更多的讨论") == LANG_ZH
    assert detect_lang("This is English only.") == LANG_EN
    assert detect_lang("我们需要 meaningful human control") == LANG_MIXED
