"""Real local tokenizer long-meeting coverage; no weights/GPU inference."""
import argparse
from pathlib import Path
import os
import sys
from uuid import uuid4

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'src'))
from forum_meeting_worker.analysis import chunks
from forum_meeting_worker.job_io import canonical, digest
from forum_meeting_worker.models import LocalModel
from forum_meeting_worker.prompts import load_prompt


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--model', required=True)
    parser.add_argument('--output', required=True)
    args = parser.parse_args()
    path = Path(args.model).resolve(strict=True)
    output = Path(args.output)
    if output.exists():
        raise ValueError('Output already exists')
    os.environ.update(HF_HUB_OFFLINE='1', TRANSFORMERS_OFFLINE='1', HF_HUB_DISABLE_IMPLICIT_TOKEN='1')
    from transformers import AutoTokenizer
    model = LocalModel.__new__(LocalModel)
    model.fake, model.check = False, lambda: None
    model.grant = {'context_limit': 8192}
    model.tokenizer = AutoTokenizer.from_pretrained(str(path), local_files_only=True, trust_remote_code=False, token=False)
    text = ''.join(f'第{i:05d}条合成记录：本轮没有批准发布；预算是12万元。 Item {i}: do not publish without review.\n' for i in range(2500)) + '最终短尾：延期两天，并非二十天。'
    identity = str(uuid4())
    units = [{'unit_id': 'u0:0', 'text': text, 'start_utf8': 0, 'end_utf8': len(text.encode()),
        'target': {'kind': 'source', 'segment_id': identity, 'segment_revision': 1},
        'evidence': {'kind': 'source', 'session_id': str(uuid4()), 'span': {'segment_id': identity, 'segment_revision': 1}}}]
    generation = {'temperature': 0.0, 'max_output_tokens': 1024, 'safety_tokens': 128, 'max_retries': 1, 'context_limit': 8192}
    prompt = load_prompt('minutes')
    grouped = chunks(units, model, prompt, generation)
    limit = 8192 - 1024 - 128
    rows = [{'step_index': index, 'prompt_tokens': model.token_count(model.render(prompt, group)),
             'start_utf8': group[0]['start_utf8'], 'end_utf8': group[-1]['end_utf8']} for index, group in enumerate(grouped)]
    assert len(rows) > 10 and max(row['prompt_tokens'] for row in rows) <= limit
    assert rows[0]['start_utf8'] == 0 and rows[-1]['end_utf8'] == len(text.encode())
    assert all(a['end_utf8'] == b['start_utf8'] for a,b in zip(rows, rows[1:]))
    assert grouped[-1][-1]['text'].endswith('最终短尾：延期两天，并非二十天。')
    report = {'tokenizer_model_path': str(path), 'no_model_weights_loaded': True,
        'input_bytes': len(text.encode()), 'input_sha256': digest(text.encode()),
        'prompt_sha256': digest(prompt.encode()), 'budget_tokens': limit, 'chunks': rows,
        'complete_byte_coverage': True, 'final_tail_preserved': True}
    with output.open('xb') as stream:
        stream.write(canonical(report))
    print(canonical({'chunks': len(rows), 'maximum_prompt_tokens': max(r['prompt_tokens'] for r in rows), 'budget_tokens': limit,
                     'complete_byte_coverage': True, 'final_tail_preserved': True}).decode())


if __name__ == '__main__':
    main()
