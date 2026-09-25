import importlib.util
import json
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('translation_worker', Path(__file__).parents[1] / 'translation_worker.py')
w = importlib.util.module_from_spec(spec)
spec.loader.exec_module(w)

class Tests(unittest.TestCase):
    def request(self, **changes):
        return dict(id=1, source='or buy it?', context='Should we build it?', target_language='zh', max_tokens=256, temperature=0, **changes)

    def test_invalid_inputs_rejected_before_inference(self):
        base=self.request()
        for change in [dict(source=''),dict(source='x'*16385),dict(context=[]),dict(id=True),dict(temperature=float('nan')),dict(max_tokens=0),dict(max_tokens=True),dict(target_language='other'),dict(path='audio')]:
            with self.subTest(change=change):
                with self.assertRaises(ValueError): w.validate_request({**base, **change})

    def test_context_precedes_current_text_and_only_current_text_is_translated(self):
        for target in ('zh','en'):
            prompt=w.build_prompt('or buy it?', 'Should we build it?', target)
            self.assertLess(prompt.index('Should we build it?'),prompt.index('or buy it?'))
            self.assertTrue(prompt.endswith('or buy it?'))
            self.assertTrue('不要翻译背景信息' in prompt or 'only the translation of Source Text' in prompt)

    def test_streaming_snapshots_and_incomplete_generation_are_distinct(self):
        engine=w.TranslationEngine.__new__(w.TranslationEngine)
        engine.model=None
        engine.tokenizer=SimpleNamespace(apply_chat_template=lambda *a,**kw: [1])
        engine.make_sampler=lambda **kw: None
        for reason,eos in [('length',False),('stop',True)]:
            engine.stream_generate=lambda *a,**kw: iter([SimpleNamespace(text='中',finish_reason=None)]*5+[SimpleNamespace(text='文',finish_reason=reason)])
            messages=list(engine.translate(self.request()))
            self.assertEqual(messages[0]['type'],'partial')
            self.assertEqual(messages[-1]['text'],'中'*5+'文')
            self.assertEqual(messages[-1]['saw_eos'],eos)

    def test_previous_qwen_model_cannot_be_loaded_as_hymt(self):
        with tempfile.TemporaryDirectory() as folder:
            root=Path(folder)
            for file in ('config.json','tokenizer.json','tokenizer_config.json','chat_template.jinja','model.safetensors'):
                (root/file).write_text('{}')
            (root/'config.json').write_text(json.dumps({'model_type':'qwen3_5'}))
            with self.assertRaisesRegex(ValueError,'UNSUPPORTED_TRANSLATOR_ARCHITECTURE'):w.local_model(root)

if __name__=='__main__':unittest.main()
