"""Fixed ECAPA architecture; no HyperPyYAML, repository code or hub fetches."""
from __future__ import annotations

import hashlib
import io
import math
import os
from pathlib import Path
import stat

from .input import WorkerError, require


def checkpoint_bytes(grant):
    path = Path(grant['model_path']) / 'embedding_model.ckpt'
    try:
        # HF snapshots normally symlink to content-addressed blobs. This path is
        # host granted, unlike PCM paths; exact bytes, size and hash are checked.
        descriptor = os.open(path, os.O_RDONLY | os.O_NONBLOCK)
        with os.fdopen(descriptor, 'rb') as source:
            info = os.fstat(source.fileno())
            require(stat.S_ISREG(info.st_mode) and 0 < info.st_size <= 500 * 1024 * 1024,
                    'ECAPA checkpoint is not a bounded regular file.', 'MODEL_UNAVAILABLE')
            data = source.read(info.st_size + 1)
            require(len(data) == info.st_size, 'ECAPA checkpoint changed while reading.', 'MODEL_UNAVAILABLE')
    except OSError:
        raise WorkerError('MODEL_UNAVAILABLE', 'Local embedding_model.ckpt is missing or unreadable; no model will be downloaded.') from None
    require('sha256:' + hashlib.sha256(data).hexdigest() == grant['model_manifest_id'], 'ECAPA checkpoint hash differs from the host grant.', 'MODEL_MISMATCH')
    return data


def embed(samples, grant):
    data = checkpoint_bytes(grant)
    try:
        import torch
        from speechbrain.lobes.features import Fbank
        from speechbrain.lobes.models.ECAPA_TDNN import ECAPA_TDNN
        from speechbrain.processing.features import InputNormalization
    except (ImportError, OSError, RuntimeError) as exc:
        raise WorkerError('DEPENDENCY_UNAVAILABLE', 'Optional ECAPA runtime is unavailable (' + type(exc).__name__ + '); install the speaker component or keep it disabled.') from None
    try:
        torch.set_num_threads(2)
        compute_features = Fbank(n_mels=80).eval()
        normalize = InputNormalization(norm_type='sentence', std_norm=False).eval()
        encoder = ECAPA_TDNN(input_size=80, channels=[1024, 1024, 1024, 1024, 3072],
            kernel_sizes=[5, 3, 3, 3, 1], dilations=[1, 2, 3, 4, 1], attention_channels=128, lin_neurons=192).eval()
        # The official embedding checkpoint is a tensor state_dict. Never permit
        # pickled Python object construction or YAML-directed class loading.
        state = torch.load(io.BytesIO(data), map_location='cpu', weights_only=True)
        require(isinstance(state, dict) and all(isinstance(k, str) and isinstance(v, torch.Tensor) for k, v in state.items()),
                'Expected a tensor-only ECAPA state dict.', 'MODEL_MISMATCH')
        encoder.load_state_dict(state, strict=True)
        with torch.inference_mode():
            wav = torch.tensor(samples, dtype=torch.float32).unsqueeze(0) / 32768.0
            lens = torch.ones(1)
            features = normalize(compute_features(wav), lens)
            vector = encoder(features, lens).reshape(-1)
            require(vector.numel() == 192 and bool(torch.isfinite(vector).all()), 'Invalid ECAPA embedding.', 'INVALID_EMBEDDING')
            norm = float(vector.norm())
            require(math.isfinite(norm) and norm > 1e-8, 'ECAPA returned a degenerate embedding.', 'INVALID_EMBEDDING')
            return (vector / norm).tolist()
    except WorkerError:
        raise
    except Exception as exc:
        raise WorkerError('MODEL_INFERENCE_FAILED', 'Fixed ECAPA inference failed: ' + type(exc).__name__) from None
