"""Pure snapshot tasks, exact quotes, tokenizer budgets and host-confirmed resume."""
from __future__ import annotations

import json
import logging
import time
from uuid import UUID, uuid5

from .job_io import (AttemptFiles, JobError, KINDS, canonical, digest, integer,
                     require, sha, strict_json, uuid)
from .prompts import load_prompt

GENERATION_FIELDS = {'temperature', 'max_output_tokens', 'safety_tokens', 'max_retries', 'context_limit'}
PUBLIC_INPUT_KINDS = frozenset(('event_report', 'closing_brief'))


def selected_sessions(values, code='INVALID_PARAMS'):
    require(isinstance(values, list) and 1 <= len(values) <= 100,
            'Select between one and 100 explicit sessions.', code)
    try:
        ids = [uuid(value) for value in values]
    except JobError:
        raise JobError(code, 'Expected canonical non-nil session UUIDs.') from None
    require(len(set(ids)) == len(ids), 'Duplicate selected session.', code)
    return set(ids)


def validate_run(params, configuration):
    require(isinstance(params, dict) and set(params) == {
        'job_id', 'attempt', 'kind', 'session_ids', 'snapshot', 'model_profile',
        'prompt_version', 'remaining_budget_ms', 'config', 'confirmed_checkpoints'}, 'Invalid jobs.run fields.')
    uuid(params['job_id'])
    integer(params['attempt'], 1, 1000)
    integer(params['remaining_budget_ms'], 1, 24 * 3600 * 1000)
    require(params['kind'] in KINDS, 'Unsupported analysis task.')
    sessions = selected_sessions(params['session_ids'])
    require(params['kind'] in PUBLIC_INPUT_KINDS or len(sessions) == 1,
            'Source tasks support one explicitly selected session.')
    snapshot = params['snapshot']
    require(isinstance(snapshot, dict) and set(snapshot) == {'id', 'relative_path', 'sha256', 'input_cursor'}, 'Invalid snapshot descriptor.')
    uuid(snapshot['id']); sha(snapshot['sha256']); integer(snapshot['input_cursor'], 0, 2**63-1)
    grant = next((g for g in configuration.get('model_grants', []) if g['profile'] == params['model_profile']), None)
    require(grant is not None, 'Host has not granted this model profile.', 'MODEL_UNAVAILABLE')
    config = params['config']
    require(isinstance(config, dict) and set(config) == {'model_profile', 'model_manifest_id',
        'prompt_version', 'prompt_sha256', 'profile_id', 'profile_version', 'profile_sha256',
        'effective_config_hash', 'projection_policy_hash', 'generation'}, 'Invalid effective configuration.')
    require(config['model_profile'] == params['model_profile'] and config['model_manifest_id'] == grant['model_manifest_id'], 'Model grant/config mismatch.')
    require(config['profile_id'] == configuration['profile_id'] and config['profile_version'] == configuration['profile_version'], 'Host profile/config mismatch.')
    require(params['prompt_version'] == config['prompt_version'] == params['kind'] + '-v1', 'Unknown prompt version.')
    require(config['prompt_sha256'] == digest(load_prompt(params['kind']).encode()), 'Prompt content hash mismatch.')
    sha(config['projection_policy_hash']); sha(config['profile_sha256'])
    generation = config['generation']
    require(isinstance(generation, dict) and set(generation) == GENERATION_FIELDS, 'Generation must contain all five effective fields.')
    require(type(generation['temperature']) in (int, float) and 0 <= generation['temperature'] <= 1, 'Invalid temperature.')
    require(type(generation['context_limit']) is int and generation['context_limit'] == grant['context_limit'], 'Effective context_limit must match the host grant.')
    integer(generation['max_output_tokens'], 64, min(1024, grant['max_output_tokens']))
    integer(generation['safety_tokens'], 32, 4096)
    integer(generation['max_retries'], 0, 2)
    body = dict(config); body.pop('effective_config_hash')
    require(config['effective_config_hash'] == digest(canonical(body)), 'Effective configuration hash mismatch.')
    confirmed = params['confirmed_checkpoints']
    require(isinstance(confirmed, dict) and set(confirmed) == {'relative_path', 'sha256'}, 'Invalid confirmed checkpoint descriptor.')
    sha(confirmed['sha256'])
    return grant


def read_inputs(params, root):
    files = AttemptFiles(root, params['job_id'], params['attempt'])
    snapshot = strict_json(files.read(params['snapshot']['relative_path'], params['snapshot']['sha256']))
    require(isinstance(snapshot, dict) and set(snapshot) == {'schema_version', 'snapshot_id',
        'event_id', 'session_ids', 'input_cursor', 'kind', 'effective_config_hash',
        'segments', 'published_artifacts', 'input_complete'}, 'Invalid snapshot fields.', 'INVALID_SNAPSHOT')
    require(type(snapshot['schema_version']) is int and snapshot['schema_version'] == 1 and type(snapshot['input_complete']) is bool,
            'Unsupported snapshot schema.', 'INVALID_SNAPSHOT')
    uuid(snapshot['event_id'])
    for left, right in [('snapshot_id', params['snapshot']['id']), ('session_ids', params['session_ids']),
                        ('input_cursor', params['snapshot']['input_cursor']), ('kind', params['kind']),
                        ('effective_config_hash', params['config']['effective_config_hash'])]:
        require(snapshot[left] == right, 'Snapshot/job identity mismatch.', 'INVALID_SNAPSHOT')
    require(isinstance(snapshot['segments'], list) and isinstance(snapshot['published_artifacts'], list), 'Invalid snapshot inputs.', 'INVALID_SNAPSHOT')
    if params['kind'] in PUBLIC_INPUT_KINDS:
        require(not snapshot['segments'], 'Report accepts selected published artifacts only.', 'INVALID_SNAPSHOT')
        require(bool(snapshot['published_artifacts']), 'No published artifacts were selected.', 'DEPENDENCY_NOT_READY')
    else:
        require(not snapshot['published_artifacts'], 'This task accepts source segments only.', 'INVALID_SNAPSHOT')
    require(len(snapshot['segments']) + len(snapshot['published_artifacts']) <= 100000,
            'Snapshot unit limit exceeded.', 'INVALID_SNAPSHOT')
    checkpoints = strict_json(files.read(params['confirmed_checkpoints']['relative_path'], params['confirmed_checkpoints']['sha256']))
    require(isinstance(checkpoints, list) and len(checkpoints) <= 4096, 'Invalid confirmed checkpoint list.', 'INVALID_SNAPSHOT')
    confirmed = {}
    for item in checkpoints:
        require(isinstance(item, dict) and set(item) == {'job_id', 'attempt', 'step_index', 'snapshot_sha256',
            'input_sha256', 'effective_config_hash', 'result_sha256', 'result'}, 'Invalid checkpoint schema.', 'INVALID_SNAPSHOT')
        uuid(item['job_id']); integer(item['attempt'], 1, 1000)
        sha(item['snapshot_sha256'])
        require(item['effective_config_hash'] == params['config']['effective_config_hash']
                and item['result_sha256'] == digest(canonical(item['result'])), 'Confirmed checkpoint identity/hash mismatch.', 'INVALID_SNAPSHOT')
        integer(item['step_index'], 0, 4095); sha(item['input_sha256'])
        # Host supplies only core-confirmed same-event/session/kind/config
        # cache. Old job/snapshot/step identify provenance, never current
        # coverage. Exact current unit identity+revision+bytes is the key.
        key = item['input_sha256']
        require(key not in confirmed, 'Duplicate confirmed checkpoint input.', 'INVALID_SNAPSHOT')
        confirmed[key] = item
    return files, snapshot, confirmed


def input_units(snapshot):
    ready, coverage, ids = [], [], set()
    public_inputs = snapshot['kind'] in PUBLIC_INPUT_KINDS
    sessions = selected_sessions(snapshot['session_ids'], 'INVALID_SNAPSHOT')
    covered_sessions = set()
    records = snapshot['published_artifacts'] if public_inputs else snapshot['segments']
    for index, item in enumerate(records):
        require(isinstance(item, dict) and isinstance(item.get('text'), str), 'Invalid input text.', 'INVALID_SNAPSHOT')
        text = item['text']
        try:
            length = len(text.encode('utf-8'))
        except UnicodeError:
            raise JobError('INVALID_SNAPSHOT', 'Input contains invalid Unicode.') from None
        if public_inputs:
            # Only the host's reviewed public projection enters the worker.
            # The core binds owner/event and frozen revision to this snapshot;
            # no private artifact body or remote source fields are accepted.
            require(set(item) == {'artifact_id', 'revision', 'session_ids', 'kind', 'title', 'text'}
                    and item['kind'] in KINDS and item['kind'] != snapshot['kind']
                    and isinstance(item['title'], str), 'Invalid published input fields.', 'INVALID_SNAPSHOT')
            identity = uuid(item.get('artifact_id')); revision = integer(item.get('revision'), 1, 2**32-1)
            item_sessions = selected_sessions(item['session_ids'], 'INVALID_SNAPSHOT')
            require(item_sessions <= sessions, 'Cross-session report input.', 'INVALID_SNAPSHOT')
            covered_sessions.update(item_sessions)
            target = {'kind': 'artifact', 'artifact_id': identity, 'revision': revision}
            evidence = {'kind': 'artifact', 'artifact_id': identity, 'revision': revision}
            status = 'success'
        else:
            identity = uuid(item.get('segment_id')); revision = item.get('revision')
            if revision is not None:
                integer(revision, 1, 2**32-1)
            require(item.get('session_id') in snapshot['session_ids'], 'Cross-session transcript input.', 'INVALID_SNAPSHOT')
            status = item.get('status')
            require(status in ('success', 'empty', 'failed', None), 'Invalid source status.', 'INVALID_SNAPSHOT')
            require(status != 'success' or revision is not None, 'Successful source needs a revision.', 'INVALID_SNAPSHOT')
            target = {'kind': 'source', 'segment_id': identity, 'segment_revision': revision}
            evidence = {'kind': 'source', 'session_id': item['session_id'], 'span': {'segment_id': identity, 'segment_revision': revision}}
        require(identity not in ids, 'Duplicate source/artifact identity.', 'INVALID_SNAPSHOT')
        ids.add(identity)
        unit = {'unit_id': f'u{index}:0', 'text': text, 'target': target, 'evidence': evidence,
                'start_utf8': 0, 'end_utf8': length}
        if status in (None, 'failed'):
            coverage.append(cover(unit, 'failed', 'Source transcript is missing or failed.'))
        elif status == 'empty' or not text:
            require(not text, 'Empty source has text.', 'INVALID_SNAPSHOT')
            coverage.append(cover(unit, 'ignored_empty'))
        else:
            ready.append(unit)
    if public_inputs:
        require(covered_sessions == sessions, 'A selected session has no published input.', 'INVALID_SNAPSHOT')
    return ready, coverage


def cover(unit, status, reason=None):
    return {key: unit[key] for key in ('target', 'start_utf8', 'end_utf8')} | {'status': status, 'reason': reason}


def split_unit(unit, count):
    left, right = dict(unit), dict(unit)
    left['text'], right['text'] = unit['text'][:count], unit['text'][count:]
    boundary = unit['start_utf8'] + len(left['text'].encode())
    left['end_utf8'], right['start_utf8'] = boundary, boundary
    right['unit_id'] = unit['unit_id'].split(':')[0] + ':' + str(boundary)
    return left, right


def chunks(units, model, prompt, generation):
    budget = model.grant['context_limit'] - generation['max_output_tokens'] - generation['safety_tokens']
    require(model.token_count(model.render(prompt, [])) < budget, 'Prompt/schema/output reserve exhaust context.', 'INVALID_PARAMS')
    result, current = [], []
    for original in units:
        unit = original
        while unit:
            if model.token_count(model.render(prompt, current + [unit])) <= budget:
                current.append(unit)
                break
            if current:
                result.append(current); current = []
                continue
            low, high = 0, len(unit['text'])
            while low < high:
                middle = (low + high + 1) // 2
                left, _ = split_unit(unit, middle)
                if model.token_count(model.render(prompt, [left])) <= budget:
                    low = middle
                else:
                    high = middle - 1
            require(low > 0, 'One codepoint cannot fit the tokenizer budget.', 'INVALID_PARAMS')
            left, right = split_unit(unit, low)
            result.append([left])
            unit = right if right['text'] else None
            require(len(result) <= 4096, 'Token chunk limit exceeded.', 'INVALID_SNAPSHOT')
    if current:
        result.append(current)
    require(len(result) <= 4096, 'Token chunk limit exceeded.', 'INVALID_SNAPSHOT')
    return result


def citation_source(sources, unit_id, quote):
    """Resolve an ID typo only via a unique, byte-exact quote in this chunk.

    Never fuzzy-match, normalize ASR, search other meetings, or accept a repeated
    quote. The final evidence always contains the actual source ID and offsets.
    """
    unit = sources.get(unit_id)
    if unit is not None and unit['text'].count(quote) == 1:
        return unit
    matches = [(u, u['text'].count(quote)) for u in sources.values() if quote in u['text']]
    if len(matches) == 1 and matches[0][1] == 1:
        actual = matches[0][0]
        logging.getLogger('forum_meeting_worker.analysis').warning(
            'Corrected citation unit %s -> %s using unique exact quote', unit_id, actual['unit_id'])
        return actual
    raise JobError('INVALID_MODEL_OUTPUT', 'Quote must be an exact, unambiguous substring.')


def claims_from_output(output, units, namespace, step):
    require(isinstance(output, dict) and set(output) == {'claims'} and isinstance(output['claims'], list)
            and len(output['claims']) <= 8, 'Model output must contain at most eight claims.', 'INVALID_MODEL_OUTPUT')
    sources = {u['unit_id']: u for u in units}
    claims = []
    for index, item in enumerate(output['claims']):
        require(isinstance(item, dict) and set(item) == {'kind', 'text', 'citations', 'assignee', 'due'}, 'Invalid model claim fields.', 'INVALID_MODEL_OUTPUT')
        require(item['kind'] in ('fact', 'decision', 'action', 'risk', 'question', 'uncertainty')
                and isinstance(item['text'], str) and 0 < len(item['text']) <= 2048
                and isinstance(item['citations'], list) and len(item['citations']) <= 16, 'Invalid claim values.', 'INVALID_MODEL_OUTPUT')
        for key in ('assignee', 'due'):
            require(item[key] is None or isinstance(item[key], str) and 0 < len(item[key]) <= 256, 'Invalid claim attribution.', 'INVALID_MODEL_OUTPUT')
        evidence = []
        for citation in item['citations']:
            require(isinstance(citation, dict) and set(citation) == {'unit_id', 'quote'}, 'Invalid citation fields.', 'INVALID_MODEL_OUTPUT')
            quote = citation['quote']
            require(isinstance(citation['unit_id'], str) and isinstance(quote, str) and quote and len(quote) <= 4096,
                    'Citation must identify supplied input.', 'INVALID_MODEL_OUTPUT')
            unit = citation_source(sources, citation['unit_id'], quote)
            offset = unit['text'].find(quote)
            require(offset >= 0 and unit['text'].find(quote, offset + 1) < 0,
                    'Quote must be an exact, unambiguous substring.', 'INVALID_MODEL_OUTPUT')
            start = unit['start_utf8'] + len(unit['text'][:offset].encode())
            span = {'start_utf8': start, 'end_utf8': start + len(quote.encode()), 'quote': quote}
            if unit['evidence']['kind'] == 'source':
                evidence.append(unit['evidence'] | {'span': unit['evidence']['span'] | span})
            else:
                evidence.append(unit['evidence'] | span)
        require(evidence or item['kind'] in ('question', 'uncertainty'), 'Factual claims require citations.', 'INVALID_MODEL_OUTPUT')
        # No inference that a speaker is the assignee: supplied names/dates
        # must also literally appear inside the cited quotes.
        for key in ('assignee', 'due'):
            require(item[key] is None or any(item[key] in e.get('span', e)['quote'] for e in evidence),
                    'Assignee or due date is not in cited evidence.', 'INVALID_MODEL_OUTPUT')
        claims.append({'claim_id': str(uuid5(UUID(namespace), f'{step}:{index}:' + digest(canonical(item)))),
                       'kind': item['kind'], 'text': item['text'], 'evidence': evidence,
                       'grounding': 'cited' if evidence else 'unsupported',
                       'assignee': item['assignee'], 'due': item['due']})
    return claims


def validated_claims(output, units, namespace, step):
    """Keep independently valid claims, without repairing a bad quote or claim.

    Any omission marks the entire chunk's coverage incomplete. Such output is
    never checkpointed as complete and cannot become a public complete report.
    """
    try:
        return claims_from_output(output, units, namespace, step), False
    except JobError as original:
        require(isinstance(output, dict) and set(output) == {'claims'}
                and isinstance(output['claims'], list) and len(output['claims']) <= 8,
                'Invalid model envelope.', 'INVALID_MODEL_OUTPUT')
        accepted = []
        for index, claim in enumerate(output['claims']):
            try:
                accepted.extend(claims_from_output({'claims': [claim]}, units, namespace, f'{step}:{index}'))
            except JobError as error:
                logging.getLogger('forum_meeting_worker.analysis').warning(
                    'Omitted unsupported claim %s in chunk %s: %s', index, step, error)
        if not accepted:
            raise original
        return accepted, True


def insight_output(output, units, namespace, step):
    """Topics summarize the whole source chunk, independently of the two notes.

    Keep their own exact evidence so an early domain can survive even when it
    was not selected as a claim. Invalid topic metadata never discards notes.
    """
    require(isinstance(output, dict) and set(output) in ({'claims'}, {'claims', 'topics'}),
            'Invalid insight envelope.', 'INVALID_MODEL_OUTPUT')
    claim_error = None
    try:
        claims, partial = validated_claims({'claims': output['claims']}, units, namespace, step)
    except JobError as error:
        if error.code != 'INVALID_MODEL_OUTPUT':
            raise
        claim_error = error
        claims, partial = [], True
    topics = []
    supplied = output.get('topics', [])
    if not isinstance(supplied, list) or len(supplied) > 5:
        return claims, topics, True
    for item in supplied:
        try:
            require(isinstance(item, dict) and set(item) == {'label', 'citations'}
                    and isinstance(item['label'], str) and 2 <= len(item['label'].strip()) <= 12
                    and isinstance(item['citations'], list) and 1 <= len(item['citations']) <= 2,
                    'Invalid topic.', 'INVALID_MODEL_OUTPUT')
            cited = claims_from_output({'claims': [{'kind': 'fact', 'text': item['label'].strip(),
                'citations': item['citations'], 'assignee': None, 'due': None}]}, units, namespace, step)[0]
            topics.append({'label': cited['text'], 'evidence': cited['evidence']})
        except JobError:
            partial = True
    if claim_error and not topics:
        raise claim_error
    return claims, topics, partial


def execute(params, configuration, check, emit, allow_fake=False):
    grant = validate_run(params, configuration)
    files, snapshot, confirmed = read_inputs(params, configuration['job_root'])
    units, coverage = input_units(snapshot)
    require(bool(units), 'No successful nonempty text is available for analysis.', 'DEPENDENCY_NOT_READY')
    prompt, generation = load_prompt(params['kind']), params['config']['generation']
    check()
    from .models import LocalModel
    model = LocalModel(grant, check, allow_fake)
    grouped = chunks(units, model, prompt, generation)
    sections, valid = [], 0
    for step, group in enumerate(grouped):
        input_hash = digest(canonical(group))
        checkpoint = confirmed.get(input_hash)
        emit('jobs.progress', {'job_id': params['job_id'], 'attempt': params['attempt'],
            'phase': 'reusing' if checkpoint else 'analyzing', 'completed_units': step, 'total_units': len(grouped), 'wait_reason': None})
        problem = None
        topics = []
        try:
            check()
            if checkpoint:
                output = checkpoint['result']
                if params['kind'] == 'insight':
                    claims, topics, partial = insight_output(output, group, params['job_id'], step)
                    require(not partial, 'Invalid topic checkpoint.', 'INVALID_MODEL_OUTPUT')
                else:
                    claims = claims_from_output(output, group, params['job_id'], step)
            else:
                for retry in range(generation['max_retries'] + 1):
                    check()
                    raw = None
                    try:
                        rendered = model.render(prompt, group)
                        if retry:
                            repair_hint = ('Return complete claims and topics arrays: at most one claim and two broad topics, each with one short exact quote.'
                                           if params['kind'] == 'insight' else 'Return one or two concise claims only, with one short exact quote each.')
                            repair = model.render(prompt + '\nThe previous response failed validation. ' + repair_hint + ' Finish the complete JSON object. Do not enumerate all source units.', group)
                            reserve = generation['max_output_tokens'] + generation['safety_tokens']
                            if model.token_count(repair) + reserve <= generation['context_limit']:
                                rendered = repair
                        raw = model.generate(rendered, generation, group)
                        try:
                            output = strict_json(raw.encode())
                        except JobError:
                            raise JobError('INVALID_MODEL_OUTPUT', 'Model did not return strict JSON.') from None
                        if params['kind'] == 'insight':
                            claims, topics, partial = insight_output(output, group, params['job_id'], step)
                        else:
                            claims, partial = validated_claims(output, group, params['job_id'], step)
                        if partial:
                            problem = 'INVALID_MODEL_OUTPUT: unsupported claims omitted'
                            files.write(f'partial-{step}.json', {'reason': problem, 'raw_output': raw})
                        break
                    except JobError as exc:
                        logging.getLogger('forum_meeting_worker.analysis').warning('chunk %s validation: %s', step, str(exc))
                        if isinstance(raw, str):
                            files.write(f'rejected-{step}-{retry}.json', {'error_code': exc.code, 'reason': str(exc), 'raw_output': raw})
                        if exc.code != 'INVALID_MODEL_OUTPUT' or retry == generation['max_retries']:
                            raise
                if not problem:
                    checkpoint = {'job_id': params['job_id'], 'attempt': params['attempt'], 'step_index': step,
                        'snapshot_sha256': params['snapshot']['sha256'], 'input_sha256': input_hash,
                        'effective_config_hash': params['config']['effective_config_hash'],
                        'result_sha256': digest(canonical(output)), 'result': output}
                    files.write(f'checkpoint-{step}.json', checkpoint)
                    emit('jobs.checkpoint', checkpoint)
            valid += 1
            section = {'heading': f'{params["kind"]} · {step + 1}', 'claims': claims}
            if topics:
                section['topics'] = topics
            sections.append(section)
        except JobError as exc:
            if exc.code == 'CANCELLED':
                raise
            if exc.code not in ('INVALID_MODEL_OUTPUT', 'DEADLINE_EXCEEDED'):
                raise
            problem = exc.code
        coverage.extend(cover(unit, 'failed' if problem else 'processed', problem) for unit in group)
    require(valid > 0 or not grouped, 'All analysis chunks failed.', 'INVALID_MODEL_OUTPUT')
    # This is deterministic composition, not an extra unbounded synthesis
    # prompt. Every accepted chunk survives, including the final short tail.
    result = {'schema_version': 1, 'job_id': params['job_id'], 'attempt': params['attempt'],
        'snapshot_id': snapshot['snapshot_id'], 'snapshot_sha256': params['snapshot']['sha256'],
        'effective_config_hash': params['config']['effective_config_hash'],
        'content': {'title': params['kind'].replace('_', ' ') + ' (draft)', 'sections': sections},
        'coverage': {'units': coverage}}
    result_hash = files.write('result.json', result)
    return {'job_id': params['job_id'], 'attempt': params['attempt'],
        'status': 'succeeded_partial' if not snapshot['input_complete'] or any(u['status'] == 'failed' for u in coverage) else 'succeeded',
        'snapshot_id': snapshot['snapshot_id'], 'snapshot_sha256': params['snapshot']['sha256'],
        'result_ref': 'result.json', 'result_sha256': result_hash}
