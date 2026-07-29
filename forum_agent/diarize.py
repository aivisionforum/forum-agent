"""Online speaker diarization: ECAPA-TDNN embeddings + incremental centroid
clustering. Chatham House Rule: labels are Speaker A/B/C..., never names."""
import string

import numpy as np
import torch

from forum_agent.constants import (MAX_SPEAKERS, MIN_EMBED_SECONDS,
                                   SAMPLE_RATE, SPEAKER_SIM_THRESHOLD)

_LABELS = list(string.ascii_uppercase)


class Diarizer:
    def __init__(self) -> None:
        from speechbrain.inference.speaker import EncoderClassifier
        self._encoder = EncoderClassifier.from_hparams(
            source="speechbrain/spkrec-ecapa-voxceleb",
            savedir="data/ecapa_model",
            run_opts={"device": "cpu"})
        self._centroids: list[np.ndarray] = []
        self._counts: list[int] = []
        self._last_label = "A"

    def _embed(self, audio: np.ndarray) -> np.ndarray:
        wav = torch.from_numpy(audio).float().unsqueeze(0)
        emb = self._encoder.encode_batch(wav).squeeze().numpy()
        return emb / (np.linalg.norm(emb) + 1e-9)

    def assign(self, audio: np.ndarray) -> str:
        """Returns 'Speaker A'-style label for this utterance's audio."""
        if len(audio) < MIN_EMBED_SECONDS * SAMPLE_RATE:
            return f"Speaker {self._last_label}"  # too short to embed reliably
        emb = self._embed(audio)
        if self._centroids:
            sims = [float(np.dot(emb, c)) for c in self._centroids]
            best = int(np.argmax(sims))
            if sims[best] >= SPEAKER_SIM_THRESHOLD or \
                    len(self._centroids) >= MAX_SPEAKERS:
                n = self._counts[best]
                self._centroids[best] = (self._centroids[best] * n + emb) / (n + 1)
                self._centroids[best] /= np.linalg.norm(self._centroids[best]) + 1e-9
                self._counts[best] = n + 1
                self._last_label = _LABELS[best]
                return f"Speaker {self._last_label}"
        self._centroids.append(emb)
        self._counts.append(1)
        self._last_label = _LABELS[len(self._centroids) - 1]
        return f"Speaker {self._last_label}"
