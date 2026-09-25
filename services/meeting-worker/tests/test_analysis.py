"""F05/F08: actual process pipes, exact source bytes and bounded fake inference."""
from __future__ import annotations

import json
import os
from pathlib import Path
import selectors
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch
from uuid import uuid4

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'src'))
from forum_meeting_worker.analysis import (chunks, claims_from_output, validated_claims, insight_output, execute,
                                         input_units, validate_run)
from forum_meeting_worker.fingerprint import model_fingerprint
from forum_meeting_worker.job_io import AttemptFiles, JobError, canonical, digest
from forum_meeting_worker.models import LocalModel
from forum_meeting_worker.prompts import load_prompt


class Fixture:
    def __init__(self, root, texts=None, behavior=None, context=8192, kind='minutes'):
        self.root = Path(root)
        self.job, self.session, self.snapshot_id = [str(uuid4()) for _ in range(3)]
        self.directory = self.root / self.job / '1'
        self.directory.mkdir(parents=True, mode=0o700)
        self.model = self.root / 'model'
        self.model.mkdir(exist_ok=True)
        (self.model / 'config.json').write_bytes(canonical(behavior or {}))
        (self.model / 'model.safetensors').write_bytes(b'fake weights, not loadable by MLX')
        self.grant = {'profile': 'test-fake-v1', 'model_path': str(self.model),
            'model_manifest_id': model_fingerprint(self.model), 'context_limit': context, 'max_output_tokens': 1024}
        self.configuration = {'protocol_version': 1, 'instance_id': str(uuid4()),
            'job_root': str(self.root), 'profile_id': 'ai-vision-forum', 'profile_version': 'v1', 'model_grants': [self.grant]}
        config = {'model_profile': 'test-fake-v1', 'model_manifest_id': self.grant['model_manifest_id'],
            'prompt_version': kind + '-v1', 'prompt_sha256': digest(load_prompt(kind).encode()),
            'profile_id': 'ai-vision-forum', 'profile_version': 'v1', 'projection_policy_hash': 'a' * 64, 'profile_sha256': 'b' * 64,
            'generation': {'temperature': 0.0, 'max_output_tokens': 1024, 'safety_tokens': 128, 'max_retries': 1, 'context_limit': context}}
        config['effective_config_hash'] = digest(canonical(config))
        self.snapshot = {'schema_version': 1, 'snapshot_id': self.snapshot_id, 'event_id': str(uuid4()),
            'session_ids': [self.session], 'input_cursor': 42, 'kind': kind, 'input_complete': True,
            'effective_config_hash': config['effective_config_hash'], 'segments': [], 'published_artifacts': []}
        for text in texts or ['预算不是 20 万，而是 12 万。', 'Do not publish the draft before review.']:
            self.snapshot['segments'].append({'session_id': self.session, 'segment_id': str(uuid4()), 'revision': 1,
                'text': text, 'status': 'success', 'audio': {'start_sample': 0, 'end_sample': 16000,
                    'sample_rate': 16000, 'start_ms': 0, 'end_ms': 1000}, 'speaker_id': None})
        self.params = {'job_id': self.job, 'attempt': 1, 'kind': kind, 'session_ids': [self.session],
            'snapshot': {'id': self.snapshot_id, 'relative_path': 'input.json', 'sha256': '', 'input_cursor': 42},
            'model_profile': 'test-fake-v1', 'prompt_version': kind + '-v1', 'remaining_budget_ms': 10000,
            'config': config, 'confirmed_checkpoints': {'relative_path': 'checkpoints.json', 'sha256': digest(b'[]')}}
        self.save()
        (self.directory / 'checkpoints.json').write_bytes(b'[]')

    def save(self):
        data = canonical(self.snapshot)
        (self.directory / 'input.json').write_bytes(data)
        self.params['snapshot']['sha256'] = digest(data)

    def published(self, sessions=None):
        sessions = sessions or [self.session]
        self.params['session_ids'] = list(sessions)
        self.snapshot['session_ids'] = list(sessions)
        self.snapshot['segments'] = []
        self.snapshot['published_artifacts'] = [
            {'artifact_id': str(uuid4()), 'revision': index + 2,
             'session_ids': [session], 'kind': 'minutes', 'title': f'Reviewed room {index}',
             'text': f'会场 {index}：公开预算为 {12 + index} 万元，尚未确认负责人。'}
            for index, session in enumerate(sessions)]
        self.save()

    def run(self):
        self.messages = []
        return execute(self.params, self.configuration, lambda: None,
                       lambda method, params: self.messages.append((method, params)), True)


class AnalysisTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)

    def test_topics_cover_earlier_source_independently_of_latest_claim_and_survive_resume(self):
        f = Fixture(self.tmp.name, texts=['课堂设备能否提高孩子的学习兴趣？', '政府需要改进公共服务。'], kind='insight')
        output = {'claims': [{'kind': 'question', 'text': '如何改进公共服务？',
            'citations': [{'unit_id': 'u1:0', 'quote': '政府需要改进公共服务'}], 'assignee': None, 'due': None}],
            'topics': [{'label': '教育', 'citations': [{'unit_id': 'u0:0', 'quote': '提高孩子的学习兴趣'}]},
                       {'label': '政府', 'citations': [{'unit_id': 'u1:0', 'quote': '政府需要改进公共服务'}]}]}
        with patch.object(LocalModel, 'generate', return_value=json.dumps(output, ensure_ascii=False)):
            self.assertEqual(f.run()['status'], 'succeeded')
        section = json.loads((f.directory / 'result.json').read_text())['content']['sections'][0]
        self.assertEqual([t['label'] for t in section['topics']], ['教育', '政府'])
        self.assertEqual(section['topics'][0]['evidence'][0]['span']['segment_id'], f.snapshot['segments'][0]['segment_id'])
        checkpoints = [p for method, p in f.messages if method == 'jobs.checkpoint']
        second = f.root / f.job / '2'; second.mkdir(mode=0o700)
        (second / 'input.json').write_bytes((f.directory / 'input.json').read_bytes())
        (second / 'checkpoints.json').write_bytes(canonical(checkpoints))
        f.params['attempt'] = 2
        f.params['confirmed_checkpoints']['sha256'] = digest(canonical(checkpoints))
        with patch.object(LocalModel, 'generate', side_effect=AssertionError('Should reuse topics')):
            self.assertEqual(f.run()['status'], 'succeeded')
        self.assertEqual(json.loads((second / 'result.json').read_text())['content']['sections'][0]['topics'], section['topics'])

    def test_topics_require_exact_evidence_but_do_not_need_a_selected_claim(self):
        f = Fixture(self.tmp.name, texts=['课堂设备能否提高孩子的学习兴趣？'], kind='insight')
        units, _ = input_units(f.snapshot)
        output = {'claims': [], 'topics': [{'label': '教育', 'citations': [{'unit_id': 'u0:0', 'quote': '孩子的学习兴趣'}]}]}
        claims, topics, partial = insight_output(output, units, f.job, 0)
        self.assertEqual(claims, [])
        self.assertEqual(topics[0]['label'], '教育')
        self.assertFalse(partial)
        with patch.object(LocalModel, 'generate', return_value=json.dumps(output, ensure_ascii=False)):
            self.assertEqual(f.run()['status'], 'succeeded')
        for citations in [[], [{'unit_id': 'u0:0', 'quote': '从未讨论医疗'}]]:
            claims, topics, partial = insight_output({'claims': [], 'topics': [{'label': '医疗', 'citations': citations}]}, units, f.job, 0)
            self.assertEqual(topics, [])
            self.assertTrue(partial)

    def test_small_talk_has_no_topic_and_legacy_output_remains_readable(self):
        f = Fixture(self.tmp.name, texts=['Thank you. Right.'], kind='insight')
        units, _ = input_units(f.snapshot)
        for output in [{'claims': []}, {'claims': [], 'topics': []}]:
            self.assertEqual(insight_output(output, units, f.job, 0), ([], [], False))

    def test_invalid_insight_cannot_discard_an_independently_cited_topic(self):
        f = Fixture(self.tmp.name, texts=['学校需要改善课堂教学。'], kind='insight')
        units, _ = input_units(f.snapshot)
        output = {'claims': [{'kind': 'fact', 'text': '不实结论', 'citations': [], 'assignee': None, 'due': None}],
                  'topics': [{'label': '教育', 'citations': [{'unit_id': 'u0:0', 'quote': '学校需要改善课堂教学'}]}]}
        claims, topics, partial = insight_output(output, units, f.job, 0)
        self.assertEqual(claims, [])
        self.assertEqual(topics[0]['label'], '教育')
        self.assertTrue(partial)

    def test_exact_utf8_sources_and_no_transcript_mutation(self):
        f = Fixture(self.tmp.name)
        before = (f.directory / 'input.json').read_bytes()
        result = f.run()
        self.assertEqual(result['status'], 'succeeded')
        content = json.loads((f.directory / 'result.json').read_text())
        spans = [c['evidence'][0]['span'] for s in content['content']['sections'] for c in s['claims']]
        for span in spans:
            source = next(s for s in f.snapshot['segments'] if s['segment_id'] == span['segment_id'])
            self.assertEqual(source['text'].encode()[span['start_utf8']:span['end_utf8']].decode(), span['quote'])
        self.assertEqual(before, (f.directory / 'input.json').read_bytes())
        self.assertEqual(len(content['coverage']['units']), 2)

    def test_long_meeting_split_includes_tail_and_all_byte_ranges(self):
        f = Fixture(self.tmp.name, texts=[''.join(f'中文📝第{i:04d}号明确不批准。' for i in range(700)) + '最后一段预算12万元。'], context=3000)
        result = f.run()
        self.assertEqual(result['status'], 'succeeded')
        content = json.loads((f.directory / 'result.json').read_text())
        coverage = sorted(content['coverage']['units'], key=lambda v: v['start_utf8'])
        self.assertGreater(len(coverage), 10)
        self.assertEqual(coverage[0]['start_utf8'], 0)
        self.assertEqual(coverage[-1]['end_utf8'], len(f.snapshot['segments'][0]['text'].encode()))
        self.assertTrue(all(a['end_utf8'] == b['start_utf8'] for a,b in zip(coverage, coverage[1:])))

    def test_last_short_chunk_and_partial_failure_are_visible(self):
        texts = [f'Unique statement {i:04d}: do not approve the proposed 12 units. ' + ('x' * 400) for i in range(30)] + ['TAIL 99.']
        f = Fixture(self.tmp.name, texts=texts, behavior={'fail_contains': '0010'}, context=4096)
        result = f.run()
        self.assertEqual(result['status'], 'succeeded_partial')
        coverage = json.loads((f.directory / 'result.json').read_text())['coverage']['units']
        self.assertEqual({u['target']['segment_id'] for u in coverage}, {s['segment_id'] for s in f.snapshot['segments']})
        tail = f.snapshot['segments'][-1]
        self.assertTrue(any(u['target']['segment_id'] == tail['segment_id'] and u['status'] == 'processed' for u in coverage))

    def test_confirmed_checkpoint_resume_and_config_fence(self):
        f = Fixture(self.tmp.name)
        f.run()
        checkpoints = [p for method,p in f.messages if method == 'jobs.checkpoint']
        second = f.root / f.job / '2'; second.mkdir(mode=0o700)
        (second / 'input.json').write_bytes((f.directory / 'input.json').read_bytes())
        (second / 'checkpoints.json').write_bytes(canonical(checkpoints))
        f.params['attempt'] = 2
        f.params['confirmed_checkpoints']['sha256'] = digest(canonical(checkpoints))
        result = f.run()
        self.assertEqual(result['status'], 'succeeded')
        self.assertTrue(any(m == 'jobs.progress' and p['phase'] == 'reusing' for m,p in f.messages))
        self.assertFalse(any(m == 'jobs.checkpoint' for m,_ in f.messages))
        checkpoints[0]['result_sha256'] = 'b' * 64
        (second / 'checkpoints.json').write_bytes(canonical(checkpoints))
        f.params['confirmed_checkpoints']['sha256'] = digest(canonical(checkpoints))
        with self.assertRaisesRegex(JobError, 'identity/hash mismatch'):
            f.run()

    def test_unconfirmed_disk_checkpoint_is_never_reused(self):
        f = Fixture(self.tmp.name)
        (f.directory / 'checkpoint-0.json').write_text('{}')
        with self.assertRaises(FileExistsError):
            f.run()  # Immutable attempt is rejected, not trusted/replaced.

    def test_paths_symlinks_hash_change_and_unknown_config_rejected(self):
        f = Fixture(self.tmp.name)
        for name in ('../input.json', '/etc/passwd', 'a/b.json'):
            with self.assertRaises(JobError):
                AttemptFiles(str(f.root), f.job, 1).read(name, f.params['snapshot']['sha256'])
        (f.directory / 'alias.json').symlink_to(f.directory / 'input.json')
        with self.assertRaises(OSError):
            AttemptFiles(str(f.root), f.job, 1).read('alias.json', f.params['snapshot']['sha256'])
        (f.directory / 'input.json').write_text('{}')
        with self.assertRaisesRegex(JobError, 'hash mismatched'):
            f.run()
        f.params['config']['generation']['unhashed_option'] = True
        with self.assertRaises(JobError):
            validate_run(f.params, f.configuration)

    def test_model_fingerprint_includes_template_and_rejects_switch(self):
        f = Fixture(self.tmp.name)
        (f.model / 'chat_template.jinja').write_text('different inference template')
        with self.assertRaisesRegex(JobError, 'identity differs'):
            f.run()

    def test_input_gaps_partial_and_missing_source_coverage(self):
        f = Fixture(self.tmp.name)
        f.snapshot['input_complete'] = False
        f.snapshot['segments'][1].update(status=None, revision=None, text='')
        f.save()
        self.assertEqual(f.run()['status'], 'succeeded_partial')
        coverage = json.loads((f.directory / 'result.json').read_text())['coverage']['units']
        self.assertTrue(any(u['status'] == 'failed' and u['target']['segment_revision'] is None for u in coverage))

    def test_report_cannot_read_raw_sources_or_other_session(self):
        f = Fixture(self.tmp.name, kind='event_report')
        with self.assertRaisesRegex(JobError, 'published artifacts only'):
            f.run()
        f.snapshot['segments'] = []
        f.snapshot['published_artifacts'] = [{'artifact_id': str(uuid4()), 'revision': 1,
            'session_ids': [str(uuid4())], 'kind': 'minutes', 'title': 'other', 'text': 'not in selected session'}]
        f.save()
        with self.assertRaisesRegex(JobError, 'Cross-session'):
            f.run()

    def test_finite_retry_does_not_change_output_contract(self):
        f = Fixture(self.tmp.name, behavior={'invalid_first': True})
        prompts = []
        generate = LocalModel.generate
        def record(model, rendered, generation, units):
            prompts.append(rendered)
            return generate(model, rendered, generation, units)
        with patch.object(LocalModel, 'generate', new=record):
            self.assertEqual(f.run()['status'], 'succeeded')
        self.assertEqual(len(prompts), 2)
        self.assertNotEqual(prompts[0], prompts[1])
        self.assertIn('previous response failed validation', prompts[1])

    def test_one_bad_quote_does_not_discard_valid_claims_or_claim_complete_coverage(self):
        f = Fixture(self.tmp.name, texts=['预算还没有批准。', '需要周五确认名单。'], kind='insight')
        good = {'kind': 'fact', 'text': '预算尚未批准', 'citations': [{'unit_id': 'u0:0', 'quote': '还没有批准'}], 'assignee': None, 'due': None}
        bad = good | {'text': '虚构金额', 'citations': [{'unit_id': 'u1:0', 'quote': '预算二十万'}]}
        with patch.object(LocalModel, 'generate', return_value=json.dumps({'claims': [good, bad]})):
            self.assertEqual(f.run()['status'], 'succeeded_partial')
        result = json.loads((f.directory / 'result.json').read_text())
        self.assertEqual([c['text'] for c in result['content']['sections'][0]['claims']], ['预算尚未批准'])
        self.assertTrue(all(u['status'] == 'failed' for u in result['coverage']['units']))
        self.assertFalse(any(method == 'jobs.checkpoint' for method, _ in f.messages))
        units, _ = input_units(f.snapshot)
        with self.assertRaises(JobError):
            validated_claims({'claims': [bad]}, units, f.job, 0)

    def test_all_six_task_routes_and_published_report_evidence(self):
        from forum_meeting_worker.job_io import KINDS
        for kind in KINDS:
            directory = Path(self.tmp.name) / kind; directory.mkdir(mode=0o700)
            f = Fixture(directory, kind=kind)
            if kind in ('event_report', 'closing_brief'):
                f.published()
            self.assertEqual(f.run()['status'], 'succeeded')
            if kind in ('event_report', 'closing_brief'):
                artifact = json.loads((f.directory / 'result.json').read_text())
                self.assertEqual(artifact['content']['sections'][0]['claims'][0]['evidence'][0]['kind'], 'artifact')

    def test_two_room_report_and_closing_preserve_frozen_public_evidence(self):
        for kind in ('event_report', 'closing_brief'):
            with self.subTest(kind=kind):
                root = Path(self.tmp.name) / kind
                root.mkdir(mode=0o700)
                f = Fixture(root, kind=kind)
                f.published([str(uuid4()), f.session])
                before = (f.directory / 'input.json').read_bytes()
                self.assertEqual(f.run()['status'], 'succeeded')
                self.assertEqual(before, (f.directory / 'input.json').read_bytes())
                result = json.loads((f.directory / 'result.json').read_text())
                expected = {(a['artifact_id'], a['revision']) for a in f.snapshot['published_artifacts']}
                self.assertEqual({(u['target']['artifact_id'], u['target']['revision'])
                                  for u in result['coverage']['units']}, expected)
                evidence = [e for section in result['content']['sections']
                            for claim in section['claims'] for e in claim['evidence']]
                self.assertEqual({(e['artifact_id'], e['revision']) for e in evidence}, expected)
                for e in evidence:
                    self.assertEqual(e['kind'], 'artifact')
                    source = next(a for a in f.snapshot['published_artifacts'] if a['artifact_id'] == e['artifact_id'])
                    self.assertEqual(source['text'].encode()[e['start_utf8']:e['end_utf8']].decode(), e['quote'])

    def test_public_scope_coverage_private_fields_and_mixed_inputs_rejected(self):
        for kind in ('event_report', 'closing_brief'):
            f = Fixture(self.tmp.name, kind=kind)
            source = f.snapshot['segments'][0]
            f.published([f.session, str(uuid4())])
            valid = json.loads(json.dumps(f.snapshot))
            for mutation in ('missing_room', 'unselected_room', 'empty_scope', 'duplicate_scope',
                             'private_fields', 'raw_source', 'same_kind'):
                with self.subTest(kind=kind, mutation=mutation):
                    f.snapshot = json.loads(json.dumps(valid))
                    item = f.snapshot['published_artifacts'][0]
                    if mutation == 'missing_room':
                        f.snapshot['published_artifacts'].pop()
                    elif mutation == 'unselected_room':
                        item['session_ids'] = [str(uuid4())]
                    elif mutation == 'empty_scope':
                        item['session_ids'] = []
                    elif mutation == 'duplicate_scope':
                        item['session_ids'] *= 2
                    elif mutation == 'private_fields':
                        item['private_transcript'] = 'must never enter model'
                    elif mutation == 'raw_source':
                        f.snapshot['segments'] = [source]
                    elif mutation == 'same_kind':
                        item['kind'] = kind
                    f.save()
                    with self.assertRaises(JobError):
                        f.run()
                    self.assertFalse((f.directory / 'result.json').exists())

    def test_session_bounds_and_single_session_source_task_boundary(self):
        from forum_meeting_worker.job_io import KINDS
        for kind in KINDS:
            f = Fixture(self.tmp.name, kind=kind)
            if kind in ('event_report', 'closing_brief'):
                f.published([f.session, str(uuid4())])
                validate_run(f.params, f.configuration)
                f.params['session_ids'] = [str(uuid4()) for _ in range(100)]
                validate_run(f.params, f.configuration)
            else:
                f.params['session_ids'].append(str(uuid4()))
                with self.assertRaisesRegex(JobError, 'Source tasks'):
                    validate_run(f.params, f.configuration)
            for sessions in ([], [f.session, f.session], ['not-a-uuid'], [str(uuid4()) for _ in range(101)]):
                f.params['session_ids'] = sessions
                with self.assertRaises(JobError):
                    validate_run(f.params, f.configuration)

    def test_source_tasks_reject_public_input_and_snapshot_identity_change(self):
        f = Fixture(self.tmp.name)
        f.published()
        with self.assertRaisesRegex(JobError, 'source segments only'):
            f.run()
        f = Fixture(self.tmp.name, kind='closing_brief')
        f.published([f.session, str(uuid4())])
        f.snapshot['session_ids'].reverse()
        f.save()
        with self.assertRaisesRegex(JobError, 'identity mismatch'):
            f.run()

    def test_quote_mutation_and_invented_attribution_rejected(self):
        f = Fixture(self.tmp.name, texts=['没有确定负责人，也没有承诺日期。'])
        units, _ = input_units(f.snapshot)
        claim = {'kind': 'fact', 'text': 'No owner', 'citations': [{'unit_id': 'u0:0', 'quote': '没有确定负责人。'}], 'assignee': None, 'due': None}
        with self.assertRaisesRegex(JobError, 'exact, unambiguous'):
            claims_from_output({'claims': [claim]}, units, f.job, 0)
        claim['citations'][0]['quote'] = '没有确定负责人'
        claim['assignee'] = 'Alice'
        with self.assertRaisesRegex(JobError, 'not in cited evidence'):
            claims_from_output({'claims': [claim]}, units, f.job, 0)

    def test_wrong_unit_is_repaired_only_by_unique_exact_quote_with_utf8_offsets(self):
        f = Fixture(self.tmp.name, texts=['另一个人的发言。', '📝结论：预算是十二万元。'])
        units, _ = input_units(f.snapshot)
        claim = {'kind': 'fact', 'text': '预算十二万元', 'citations': [
            {'unit_id': 'u0:0', 'quote': '预算是十二万元'}], 'assignee': None, 'due': None}
        result = claims_from_output({'claims': [claim]}, units, f.job, 0)
        span = result[0]['evidence'][0]['span']
        self.assertEqual(span['segment_id'], f.snapshot['segments'][1]['segment_id'])
        self.assertEqual(span['start_utf8'], len('📝结论：'.encode()))
        self.assertEqual(f.snapshot['segments'][1]['text'].encode()[span['start_utf8']:span['end_utf8']].decode(), '预算是十二万元')

    def test_citation_repair_rejects_ambiguous_repeated_or_fuzzy_quotes(self):
        for texts, quote in [(['预算十二万。', '预算十二万。'], '预算十二万'),
                             (['预算十二万。预算十二万。'], '预算十二万'),
                             (['预算十二万。'], '预算是十二万')]:
            f = Fixture(self.tmp.name, texts=texts)
            units, _ = input_units(f.snapshot)
            claim = {'kind': 'fact', 'text': '预算', 'citations': [
                {'unit_id': 'wrong-unit', 'quote': quote}], 'assignee': None, 'due': None}
            with self.assertRaisesRegex(JobError, 'exact, unambiguous'):
                claims_from_output({'claims': [claim]}, units, f.job, 0)

    def test_32b_grant_is_explicit_and_fake_not_a_production_backend(self):
        from forum_meeting_worker.job_io import validate_grants
        f = Fixture(self.tmp.name)
        with self.assertRaises(JobError):
            validate_grants([f.grant], False)
        batch = f.grant | {'profile': 'meeting-32b-v1'}
        self.assertEqual(validate_grants([batch], False), [batch])
        f.configuration['model_grants'] = []
        with self.assertRaisesRegex(JobError, 'not granted'):
            validate_run(f.params, f.configuration)

    def test_no_usable_source_does_not_create_placeholder_minutes(self):
        f = Fixture(self.tmp.name)
        for source in f.snapshot['segments']:
            source.update(text='', status='failed')
        f.save()
        with self.assertRaisesRegex(JobError, 'No successful nonempty text'):
            f.run()
        self.assertFalse((f.directory / 'result.json').exists())

    def test_context_change_changes_effective_hash_and_rejects_old_config(self):
        f = Fixture(self.tmp.name)
        old = f.params['config']['effective_config_hash']
        f.grant['context_limit'] = 4096
        with self.assertRaisesRegex(JobError, 'context_limit'):
            validate_run(f.params, f.configuration)
        f.params['config']['generation']['context_limit'] = 4096
        with self.assertRaisesRegex(JobError, 'configuration hash'):
            validate_run(f.params, f.configuration)
        body = dict(f.params['config']); body.pop('effective_config_hash')
        f.params['config']['effective_config_hash'] = digest(canonical(body))
        self.assertNotEqual(old, f.params['config']['effective_config_hash'])
        validate_run(f.params, f.configuration)

    def test_appended_snapshot_reuses_exact_full_chunks_but_revision_change_invalidates(self):
        texts = [f'Source {i}: not approved, budget 12. ' + 'x' * 160 for i in range(18)]
        first = Fixture(self.tmp.name, texts=texts, context=4096)
        first.run()
        confirmed = [p for method,p in first.messages if method == 'jobs.checkpoint']
        def later(change_revision):
            current = Fixture(self.tmp.name, context=4096)
            current.session = first.session
            current.snapshot['session_ids'] = [first.session]
            current.params['session_ids'] = [first.session]
            current.snapshot['segments'] = json.loads(json.dumps(first.snapshot['segments']))
            current.snapshot['segments'].append({'session_id': first.session, 'segment_id': str(uuid4()),
                'revision': 1, 'text': 'New tail: do not forget these 2 days.', 'status': 'success',
                'audio': first.snapshot['segments'][0]['audio'], 'speaker_id': None})
            if change_revision:
                current.snapshot['segments'][0]['revision'] = 2
                current.snapshot['segments'][0]['text'] = 'Corrected source: budget 15, not 12.'
            current.save()
            (current.directory / 'checkpoints.json').write_bytes(canonical(confirmed))
            current.params['confirmed_checkpoints']['sha256'] = digest(canonical(confirmed))
            current.run()
            progress = [p for m,p in current.messages if m == 'jobs.progress']
            content = json.loads((current.directory / 'result.json').read_text())
            self.assertEqual(content['job_id'], current.job)
            self.assertEqual(content['snapshot_id'], current.snapshot_id)
            self.assertTrue(any(p['phase'] == 'reusing' for p in progress))
            self.assertTrue(any(p['phase'] == 'analyzing' for p in progress))
            if change_revision:
                self.assertEqual(progress[0]['phase'], 'analyzing')
                source = current.snapshot['segments'][0]
                self.assertTrue(any(u['target']['segment_id'] == source['segment_id'] and u['target']['segment_revision'] == 2 for u in content['coverage']['units']))
            else:
                self.assertEqual(progress[0]['phase'], 'reusing')
        later(False)
        later(True)


class PipeTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(); self.addCleanup(self.tmp.cleanup)
        public_kind = {'test_two_room_report_through_compute_process': 'event_report',
                       'test_two_room_closing_through_compute_process': 'closing_brief'}.get(self._testMethodName)
        immediate = public_kind or self._testMethodName == 'test_successful_compute_exits_cleanly'
        delay = 0 if immediate else 0.5 if self._testMethodName == 'test_pause_retains_attempt_and_resumes_without_restart' else 10
        self.f = Fixture(self.tmp.name, behavior={'delay_seconds': delay}, kind=public_kind or 'minutes')
        if public_kind:
            self.f.published([str(uuid4()), self.f.session])
        if self._testMethodName == 'test_uncooperative_compute_is_reaped_before_cancel_result':
            (self.f.model / 'config.json').write_bytes(canonical({'uninterruptible_delay_seconds': 10}))
            fingerprint = model_fingerprint(self.f.model)
            self.f.grant['model_manifest_id'] = fingerprint
            config = self.f.params['config']
            config['model_manifest_id'] = fingerprint
            config.pop('effective_config_hash')
            config['effective_config_hash'] = digest(canonical(config))
            self.f.snapshot['effective_config_hash'] = config['effective_config_hash']
            self.f.save()
        environment = os.environ.copy()
        environment['PYTHONPATH'] = str(Path(__file__).resolve().parents[1] / 'src')
        self.process = subprocess.Popen([sys.executable, '-m', 'forum_meeting_worker', '--allow-test-models'],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=environment, bufsize=0)
        self.addCleanup(self.cleanup)
        self.pending = bytearray()
        self.send('initialize', 'init', self.f.configuration)
        self.assertEqual(self.read()['id'], 'init')

    def cleanup(self):
        if self.process.poll() is None:
            self.send('shutdown', 'shutdown', {})
            self.process.wait(timeout=5)
        for pipe in (self.process.stdin, self.process.stdout, self.process.stderr):
            pipe.close()

    def send(self, method, identity, params):
        self.process.stdin.write(canonical({'jsonrpc': '2.0', 'id': identity, 'method': method, 'params': params}) + b'\n')

    def read(self, seconds=5):
        deadline = time.monotonic() + seconds
        with selectors.DefaultSelector() as selector:
            selector.register(self.process.stdout, selectors.EVENT_READ)
            while b'\n' not in self.pending:
                if not selector.select(max(0, deadline - time.monotonic())):
                    self.fail('timed out reading worker')
                chunk = os.read(self.process.stdout.fileno(), 65536)
                if not chunk:
                    self.fail('worker EOF: ' + self.process.stderr.read().decode())
                self.pending.extend(chunk)
        line, _, tail = self.pending.partition(b'\n'); self.pending = bytearray(tail)
        return json.loads(line)

    def test_ping_cancel_while_model_computes_and_exit(self):
        self.send('jobs.run', 'run', self.f.params)
        while self.read().get('method') != 'jobs.progress':
            pass
        started = time.monotonic()
        self.send('health.ping', 'ping', {})
        self.assertEqual(self.read()['id'], 'ping')
        self.assertLess(time.monotonic() - started, 0.5)
        self.send('jobs.cancel', 'cancel', {'job_id': self.f.job, 'attempt': 1})
        responses = {}
        while 'run' not in responses or 'cancel' not in responses:
            message = self.read(); responses[message.get('id')] = message
        self.assertEqual(responses['cancel']['result']['status'], 'cancel_requested')
        self.assertEqual(responses['run']['error']['data']['code'], 'CANCELLED')
        self.assertFalse((self.f.directory / 'result.json').exists())

    def test_total_deadline_is_not_reset_by_progress(self):
        self.f.params['remaining_budget_ms'] = 200
        self.send('jobs.run', 'run', self.f.params)
        started = time.monotonic()
        while True:
            message = self.read()
            if message.get('id') == 'run':
                self.assertEqual(message['error']['data']['code'], 'DEADLINE_EXCEEDED')
                break
        self.assertLess(time.monotonic() - started, 2)

    def pause_compute(self):
        self.send('jobs.run', 'run', self.f.params)
        while self.read().get('method') != 'jobs.progress':
            pass
        self.send('jobs.set_paused', 'pause', {'job_id': self.f.job, 'attempt': 1, 'paused': True})
        acknowledged = parked = False
        while not (acknowledged and parked):
            message = self.read()
            if message.get('id') == 'pause':
                self.assertEqual(message['result']['status'], 'flow_requested')
                acknowledged = True
            if message.get('method') == 'jobs.flow':
                self.assertEqual(message['params'], {'job_id': self.f.job, 'attempt': 1, 'paused': True})
                parked = True

    def test_pause_retains_attempt_and_resumes_without_restart(self):
        self.pause_compute()
        self.send('jobs.set_paused', 'wrong', {'job_id': self.f.job, 'attempt': 2, 'paused': False})
        self.assertIn('error', self.read())
        time.sleep(0.6)
        self.assertFalse((self.f.directory / 'result.json').exists())
        self.send('health.ping', 'ping', {})
        self.assertEqual(self.read()['id'], 'ping')
        self.send('jobs.set_paused', 'resume', {'job_id': self.f.job, 'attempt': 1, 'paused': False})
        resumed = False
        while True:
            message = self.read()
            if message.get('method') == 'jobs.flow':
                self.assertFalse(message['params']['paused'])
                resumed = True
            if message.get('id') == 'run':
                self.assertEqual(message['result']['attempt'], 1)
                self.assertEqual(message['result']['status'], 'succeeded')
                break
        self.assertTrue(resumed)

    def test_cancel_wakes_paused_compute(self):
        self.pause_compute()
        self.send('jobs.cancel', 'cancel', {'job_id': self.f.job, 'attempt': 1})
        while True:
            message = self.read()
            if message.get('id') == 'run':
                self.assertEqual(message['error']['data']['code'], 'CANCELLED')
                break

    def test_pause_does_not_extend_total_deadline(self):
        self.f.params['remaining_budget_ms'] = 600
        self.pause_compute()
        while True:
            message = self.read()
            if message.get('id') == 'run':
                self.assertEqual(message['error']['data']['code'], 'DEADLINE_EXCEEDED')
                break

    def test_successful_compute_exits_cleanly(self):
        self.send('jobs.run', 'run', self.f.params)
        while True:
            message = self.read()
            if message.get('id') == 'run':
                self.assertIn('result', message, message)
                self.assertEqual(message['result']['status'], 'succeeded')
                break
        self.send('shutdown', 'end', {})
        self.assertEqual(self.read()['id'], 'end')
        self.assertEqual(self.process.wait(timeout=3), 0)

    def test_two_room_report_through_compute_process(self):
        self.test_successful_compute_exits_cleanly()
        result = json.loads((self.f.directory / 'result.json').read_text())
        self.assertEqual(len(result['coverage']['units']), 2)
        self.assertTrue(all(u['target']['kind'] == 'artifact' for u in result['coverage']['units']))

    def test_two_room_closing_through_compute_process(self):
        self.test_two_room_report_through_compute_process()

    def test_uncooperative_compute_is_reaped_before_cancel_result(self):
        self.send('jobs.run', 'run', self.f.params)
        while self.read().get('method') != 'jobs.progress':
            pass
        started = time.monotonic()
        self.send('jobs.cancel', 'cancel', {'job_id': self.f.job, 'attempt': 1})
        while True:
            message = self.read()
            if message.get('id') == 'run':
                self.assertEqual(message['error']['data']['code'], 'CANCELLED')
                break
        self.assertLess(time.monotonic() - started, 2)
        self.send('shutdown', 'end', {})
        while self.read().get('id') != 'end':
            pass
        self.assertEqual(self.process.wait(timeout=3), 0)


if __name__ == '__main__':
    unittest.main()
