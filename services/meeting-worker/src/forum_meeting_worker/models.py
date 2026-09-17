"""Local-only MLX and an explicit test backend. Imported only in compute child."""
from __future__ import annotations

import json
import os
from pathlib import Path
import time

from .fingerprint import model_fingerprint
from .job_io import JobError, require


class LocalModel:
    def __init__(self, grant, check, allow_fake=False):
        self.grant, self.check = grant, check
        path = Path(grant['model_path'])
        require(path.is_absolute() and path.is_dir(), 'Host-granted local model missing.', 'MODEL_UNAVAILABLE')
        require(model_fingerprint(path, check) == grant['model_manifest_id'],
                'Local model identity differs from host grant.', 'MODEL_UNAVAILABLE')
        self.fake = allow_fake and grant['profile'] == 'test-fake-v1'
        self.calls = 0
        if self.fake:
            self.fixture = json.loads((path / 'config.json').read_text())
            return
        require(grant['profile'] != 'test-fake-v1', 'Test backend unavailable.', 'MODEL_UNAVAILABLE')
        from .model_probe import ProbeError, validate_probe
        try:
            validate_probe({'model_path': str(path), 'prompt': 'local validation', 'max_tokens': 1}, allowed_model_types=('qwen2', 'qwen3'))
        except ProbeError:
            raise JobError('MODEL_UNAVAILABLE', 'Complete built-in local Qwen2/Qwen3 artifacts are required.') from None
        os.environ.update(HF_HUB_OFFLINE='1', TRANSFORMERS_OFFLINE='1',
                          HF_HUB_DISABLE_TELEMETRY='1', HF_HUB_DISABLE_IMPLICIT_TOKEN='1')
        try:
            import mlx.core as mx
            from mlx_lm import load
            self.model, self.tokenizer = load(str(path), tokenizer_config={
                'trust_remote_code': False, 'local_files_only': True, 'token': False})
            mx.eval(self.model.parameters())
        except Exception as exc:
            raise JobError('MODEL_UNAVAILABLE', 'Local MLX load failed: ' + type(exc).__name__) from None
        check()

    def render(self, prompt, units):
        user = json.dumps({'units': [{'unit_id': u['unit_id'], 'text': u['text']} for u in units]}, ensure_ascii=False, separators=(',', ':'))
        if self.fake:
            return prompt + '\n' + user
        return self.tokenizer.apply_chat_template(
            [{'role': 'system', 'content': prompt}, {'role': 'user', 'content': user}],
            tokenize=False, add_generation_prompt=True, enable_thinking=False)

    def token_count(self, rendered):
        self.check()
        # Fake intentionally counts UTF-8 bytes, not a production token estimate.
        return len(rendered.encode('utf-8')) if self.fake else len(self.tokenize(rendered))

    def tokenize(self, rendered):
        # Match mlx_lm.stream_generate's BOS handling, then pass these exact
        # token IDs to generation so budgeting and prefill cannot diverge.
        bos = self.tokenizer.bos_token
        return self.tokenizer.encode(rendered, add_special_tokens=bos is None or not rendered.startswith(bos))

    def generate(self, rendered, generation, units):
        self.calls += 1
        self.check()
        if self.fake:
            # Explicit test-only native-call stand-in. Production model grants
            # cannot select this backend or inject this behavior.
            if self.fixture.get('uninterruptible_delay_seconds'):
                time.sleep(min(float(self.fixture['uninterruptible_delay_seconds']), 60))
            until = time.monotonic() + min(float(self.fixture.get('delay_seconds', 0)), 60)
            while time.monotonic() < until:
                self.check()
                time.sleep(0.01)
            if any(self.fixture.get('fail_contains', '\0') in u['text'] for u in units):
                return '{invalid'
            if self.fixture.get('invalid_first') and self.calls == 1:
                return '{invalid'
            return json.dumps({'claims': [{'kind': 'fact', 'text': u['text'][:80],
                'citations': [{'unit_id': u['unit_id'], 'quote': u['text'][:80]}],
                'assignee': None, 'due': None} for u in units[:8]]}, ensure_ascii=False)
        from mlx_lm import stream_generate
        from mlx_lm.sample_utils import make_sampler
        pieces = []
        for response in stream_generate(self.model, self.tokenizer, self.tokenize(rendered),
                max_tokens=generation['max_output_tokens'],
                sampler=make_sampler(temp=generation['temperature'])):
            self.check()
            pieces.append(response.text)
        self.check()
        return ''.join(pieces)
