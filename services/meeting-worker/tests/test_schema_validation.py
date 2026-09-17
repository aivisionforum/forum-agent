from __future__ import annotations
import copy
import json
from pathlib import Path
import tempfile
import unittest
from forum_meeting_worker.schema_validation import ContractError, ContractValidator, validate_source_span

SCHEMA = Path(__file__).resolve().parents[3] / 'packages/contracts/forum.schema.json'
UID='00000000-0000-4000-8000-000000000001'

class SchemaValidationTests(unittest.TestCase):
    def setUp(self):
        self.validator=ContractValidator(SCHEMA)
        self.event={'schema_version':1,'message_id':UID,'type':'translation.requested','event_id':UID,'room_id':UID,'session_id':UID,
          'producer':{'name':'test','run_id':UID,'seq':1},
          'payload':{'translation_id':UID,'revision':1,'attempt':1,'target_language':'en','direction_epoch':1,
            'source_spans':[{'segment_id':UID,'segment_revision':1,'start_utf8':3,'end_utf8':7,'quote':'📝'}],
            'input_text':'📝','normalization_version':'identity-v1','backend':'fixture','model_manifest_id':'fixture'}}

    def test_rust_generated_schema_accepts_utf8_request(self):
        self.validator.validate_event(self.event)
        self.assertEqual(validate_source_span(self.event['payload']['source_spans'][0],'中📝文'),'📝')

    def test_schema_rejects_unknown_fields_versions_nil_ids_and_invalid_revisions(self):
        for mutate in [lambda e:e.update(schema_version=2),lambda e:e.update(unrecognized=True),
                       lambda e:e.update(message_id='00000000-0000-0000-0000-000000000000'),
                       lambda e:e['payload'].update(revision=0),lambda e:e['producer'].update(seq=0),
                       lambda e:e['producer'].update(seq=9_007_199_254_740_992),
                       lambda e:e['payload'].update(attempt=True),lambda e:e['payload'].update(source_spans=[])]:
            event=copy.deepcopy(self.event);mutate(event)
            with self.assertRaises(ContractError):self.validator.validate_event(event)

    def test_wrong_type_payload_and_nonexact_reconstruction_are_rejected(self):
        event=copy.deepcopy(self.event);event['type']='translation.final'
        with self.assertRaises(ContractError):self.validator.validate_event(event)
        event=copy.deepcopy(self.event);event['payload']['input_text']='rewritten source'
        with self.assertRaises(ContractError):self.validator.validate_event(event)
        event=copy.deepcopy(self.event);event['payload']['source_spans']*=2;event['payload']['input_text']='📝📝'
        with self.assertRaises(ContractError):self.validator.validate_event(event)

    def test_split_emoji_empty_ranges_and_changed_quotes_are_rejected(self):
        span=self.event['payload']['source_spans'][0]
        for update in [{'start_utf8':4},{'end_utf8':3},{'quote':'别'}]:
            with self.assertRaises(ContractError):validate_source_span({**span,**update},'中📝文')
        self.assertEqual(validate_source_span({**span,'start_utf8':7,'end_utf8':10,'quote':'e\u0301'},'中📝e\u0301'),'e\u0301')

    def test_schema_version_mismatch_is_not_silently_accepted(self):
        data=json.loads(SCHEMA.read_text());data['schema_version']=2
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'schema.json';path.write_text(json.dumps(data))
            with self.assertRaises(ContractError):ContractValidator(path)
