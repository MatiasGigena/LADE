#!/usr/bin/env python3
"""Generate the closed v1 wire schemas from this single reviewed declaration.

`contracts/v1/schema.json` (client <-> service) and `contracts/v1/worker-schema.json` (service <-> worker) are the
canonical artifacts consumed by the Rust and TypeScript validators; edit this file and regenerate, never the JSON.
Only the closed subset documented in docs/implementation/protocol-v1.md is emitted.
"""
import json
from pathlib import Path
import sys

root = Path(__file__).resolve().parent.parent
BUNDLE = '0.2.0'
PROTOCOL = 1


def ref(name):
    return {'$ref': f'#/$defs/{name}'}


def obj(required=None, optional=None):
    required = required or {}
    optional = optional or {}
    assert not set(required) & set(optional)
    return {'type': 'object', 'additionalProperties': False, 'required': list(required),
            'properties': {**required, **optional}}


def string(fmt=None, min_length=None, max_length=None):
    s = {'type': 'string'}
    if fmt is not None:
        s['format'] = fmt
    if min_length is not None:
        s['minLength'] = min_length
    if max_length is not None:
        s['maxLength'] = max_length
    return s


def array(items, max_items, min_items=None):
    s = {'type': 'array', 'items': items, 'maxItems': max_items}
    if min_items is not None:
        s['minItems'] = min_items
    return s


def enum(*values):
    return {'enum': list(values)}


def const(value):
    return {'const': value}


def one_of(*choices):
    return {'oneOf': list(choices)}


BOOL = {'type': 'boolean'}
ID = ref('Id')
COUNTER = ref('Counter')
TS = ref('Timestamp')
O = {'objective_id': ID}

# Shared by both schemas; generate-dtos.py asserts identical definitions for identical names.
shared = {
    'Id': string('uuid-v4'),
    'Counter': string('u64'),
    'Model': obj({'provider': string(min_length=1, max_length=128), 'id': string(min_length=1, max_length=256)}),
    'DeliveryMode': enum('prompt', 'steer', 'follow_up'),
    'ActivityRole': enum('user', 'assistant', 'thinking', 'tool', 'system'),
}

client = dict(shared)
client.update({
    'Digest': string('sha256'),
    'Timestamp': string('timestamp'),
    'Capability': string('capability'),
    'PageLimit': string('page-limit'),
    'ReadLimit': string('read-limit'),
    'GitId': one_of(obj({'algorithm': const('sha1'), 'hex': string('git-sha1')}),
                    obj({'algorithm': const('sha256'), 'hex': string('sha256')})),
    # Raw path bytes (1..=4096, canonical base64) are authoritative; display is a lossy UTF-8 label only.
    'Path': obj({'b64': string('path-b64'), 'display': string(max_length=4096)}),
    'Branch': string(min_length=1, max_length=255),
    'Title': string(min_length=1, max_length=200),
    'ShortText': string(max_length=512),
    'Label': string(max_length=256),
    'Preview': string(max_length=1024),
    'Intent': obj({'outcome': string(min_length=1), 'constraints': string(), 'acceptance': string()}),
    'Argv': array(string(max_length=4096), 256, 1),
    'Basis': one_of(obj({'kind': const('working')}),
                    obj({'kind': const('candidate'), 'candidate_id': ID}),
                    obj({'kind': const('since'), 'from_candidate_id': ID, 'to_candidate_id': ID})),
    'Start': one_of(obj({'kind': const('target')}), obj({'kind': const('commit'), 'commit': ref('GitId')})),
    'Side': enum('old', 'new'),
    'Lifecycle': enum('defined', 'active', 'frozen', 'approved', 'integrated', 'completed_no_changes', 'discarded'),
    'ExecutionState': enum('none', 'starting', 'running', 'idle', 'stopping', 'cancelled', 'failed'),
    'Execution': obj({'state': ref('ExecutionState'), 'generation': COUNTER}, {'reason': ref('ShortText')}),
    'Attention': enum('needs_attention', 'ready_for_review', 'working', 'idle', 'archived'),
    'Badge': enum('interrupted', 'delivery_uncertain', 'stale_feedback', 'verification_failed', 'verification_missing',
                  'integration_conflict', 'stale_target', 'limit_reached', 'cleaned_up', 'fixture_model',
                  'unsent_drafts'),
    'ErrorCode': enum('unauthorized', 'stale_service', 'stale_association', 'stale_execution', 'stale_reference',
                      'stale_candidate', 'stale_target', 'invalid_lifecycle', 'unsupported_content', 'version_mismatch',
                      'durability_failure', 'permission_denied', 'limit_reached', 'interrupted', 'unknown_operation',
                      'replay_expired', 'duplicate_conflict', 'not_found', 'invalid_request', 'conflict',
                      'execution_unavailable'),
    'SafeAction': enum('refresh', 'inspect_previous', 'query_operation', 'retry_read', 'explicit_continue', 'none'),
    'OperationState': enum('accepted', 'pending', 'completed', 'blocked', 'uncertain', 'unknown'),
    'Rejection': obj({'status': const('rejected'), 'code': ref('ErrorCode'), 'action': ref('SafeAction'),
                      'detail': ref('ShortText')}),
    'Empty': obj(),
    'Settings': obj({'verification_commands': array(ref('Argv'), 32)}, {'default_model': ref('Model')}),
    'Workspace': obj({'workspace_id': ID, 'path': ref('Path'), 'name': ref('Label'),
                      'branches': array(ref('Branch'), 1024)}, {'default_branch': ref('Branch')}),
    'TargetRef': obj({'branch': ref('Branch'), 'commit': ref('GitId')}),
})
summary_fields = {
    'objective_id': ID, 'workspace_id': ID, 'title': ref('Title'), 'outcome': string(),
    'lifecycle': ref('Lifecycle'), 'execution': ref('Execution'), 'attention': ref('Attention'),
    'badges': array(ref('Badge'), 16), 'updated_at': TS, 'revision': COUNTER,
}
client.update({
    'ObjectiveSummary': obj(summary_fields),
    'SessionInfo': obj({'session_id': ID, 'association_revision': COUNTER, 'execution_generation': COUNTER,
                        'model': ref('Model')}),
    'ChangeCounts': obj({'files': COUNTER, 'added': COUNTER, 'removed': COUNTER}),
    'ActionName': enum('objective.update_intent', 'execution.start', 'execution.cancel', 'execution.resume',
                       'message.send', 'feedback.submit', 'review.freeze', 'review.return', 'review.verify',
                       'review.approve', 'review.refresh', 'review.integrate', 'objective.complete',
                       'objective.discard', 'objective.cleanup', 'objective.delete_history'),
    'AvailableAction': obj({'action': ref('ActionName'), 'enabled': BOOL}, {'reason': ref('ShortText')}),
    'CandidateSummary': obj({'candidate_id': ID, 'ordinal': COUNTER, 'commit': ref('GitId'),
                             'base_target_commit': ref('GitId'),
                             'status': enum('current', 'historical', 'invalidated'),
                             'files': COUNTER, 'added': COUNTER, 'removed': COUNTER, 'created_at': TS}),
    'VerificationSummary': obj({'verification_id': ID, 'objective_id': ID, 'candidate_id': ID,
                                'command': ref('Argv'),
                                'state': enum('running', 'passed', 'failed', 'interrupted', 'error'),
                                'started_at': TS, 'output_tail': string(), 'truncated': BOOL,
                                'provenance': const('service_observed')},
                               {'exit_code': COUNTER, 'ended_at': TS}),
    'ApprovalSummary': obj({'approval_id': ID, 'candidate_id': ID, 'target_commit': ref('GitId'),
                            'state': enum('valid', 'consumed', 'invalidated')},
                           {'override_reason': string(min_length=1, max_length=2048)}),
})
client['ObjectiveDetail'] = obj({
    **summary_fields,
    'intent': ref('Intent'), 'intent_revision': COUNTER, 'integration_target': ref('TargetRef'),
    'start_commit': ref('GitId'), 'branch': ref('Branch'),
    'candidates': array(ref('CandidateSummary'), 256), 'verifications': array(ref('VerificationSummary'), 256),
    'change_counts': ref('ChangeCounts'), 'unsent_drafts': COUNTER,
    'available_actions': array(ref('AvailableAction'), 32),
}, {'session': ref('SessionInfo'), 'approval': ref('ApprovalSummary'),
    # The Objective's Git history (retention refs, branch) was deleted on the user's explicit request.
    'history_deleted': BOOL})
client.update({
    'ActivityItem': obj({'item_id': ID, 'seq': COUNTER, 'at': TS, 'role': ref('ActivityRole'),
                         'title': ref('Preview'), 'text': string(), 'is_error': BOOL, 'truncated': BOOL},
                        {'tool_name': ref('Label')}),
    'ChangedFile': obj({'path': ref('Path'),
                        'status': enum('added', 'modified', 'deleted', 'renamed', 'type_changed'),
                        'added': COUNTER, 'removed': COUNTER, 'binary': BOOL},
                       # filtered: the path's `filter` attribute names a filter driver (Git LFS, clean/smudge);
                       # LADE never runs filters, so its raw working-tree bytes are compared (unsupported content).
                       {'old_path': ref('Path'), 'filtered': BOOL}),
    # Why part of a Change Set is not (fully) represented: a nested repository without a commit left out of
    # the capture, or filtered content stored raw. Repository-wide notices carry no path.
    'ContentNotice': obj({'kind': enum('nested_repository', 'filtered_content'), 'message': ref('ShortText')},
                         {'path': ref('Path')}),
    'ReferenceVersion': one_of(obj({'kind': const('working'), 'capture_id': ID}),
                               obj({'kind': const('candidate'), 'candidate_id': ID})),
    'Reference': obj({'artifact_digest': ref('Digest'), 'path': ref('Path'), 'version': ref('ReferenceVersion'),
                      'side': ref('Side'), 'byte_start': COUNTER, 'byte_end': COUNTER, 'selected_text': string()},
                     {'line_hint': COUNTER}),
    'Draft': obj({'draft_id': ID, 'objective_id': ID, 'destination_session_id': ID, 'reference': ref('Reference'),
                  'body': string(), 'revision': COUNTER, 'state': enum('unsent', 'submitted'), 'stale': BOOL},
                 {'input_id': ID}),
    'InputSummary': obj({'input_id': ID, 'objective_id': ID, 'operation_id': ID, 'mode': ref('DeliveryMode'),
                         'text_preview': ref('Preview'), 'accepted_at': TS,
                         'delivery': enum('queued', 'dispatched', 'received', 'uncertain', 'blocked')},
                        {'blocked_reason': ref('ShortText')}),
    'DraftRevision': obj({'draft_id': ID, 'revision': COUNTER}),
    'DiffLine': obj({'kind': enum('context', 'add', 'del'), 'text': string()},
                    {'old_no': COUNTER, 'new_no': COUNTER, 'old_byte_start': COUNTER, 'new_byte_start': COUNTER}),
    'DiffHunk': obj({'old_start': COUNTER, 'old_lines': COUNTER, 'new_start': COUNTER, 'new_lines': COUNTER,
                     'lines': array(ref('DiffLine'), 1024)}),
    # Content results: 1-based line and byte column, and the exact byte range [byte_start, byte_end) of the first
    # match in the raw file bytes (the bytes file.read retains), when the file is small enough to locate it.
    'SearchResult': obj({'kind': enum('objective', 'file', 'content'), 'preview': ref('Preview')},
                        {'objective_id': ID, 'path': ref('Path'), 'line': COUNTER, 'byte_start': COUNTER,
                         'column': COUNTER, 'byte_end': COUNTER}),
    # Payloads (request name -> payload def).
    'ObjectiveSelector': obj(O),
    'ActivityPageQuery': obj({**O, 'limit': ref('PageLimit')}, {'before': COUNTER}),
    'ChangesListQuery': obj({**O, 'basis': ref('Basis')}),
    'FileReadQuery': obj({**O, 'basis': ref('Basis'), 'path': ref('Path'), 'side': ref('Side'), 'offset': COUNTER,
                          'limit': ref('ReadLimit')}),
    'DiffGetQuery': obj({**O, 'basis': ref('Basis'), 'path': ref('Path')}),
    'SearchQuery': obj({'workspace_id': ID, 'scope': enum('objectives', 'files', 'content'),
                        'query': string(min_length=1, max_length=1024), 'limit': ref('PageLimit')},
                       {'objective_id': ID, 'basis': ref('Basis')}),
    'OperationGetQuery': obj({'operation_id': ID}),
    'WorkspaceOpenCommand': obj({'path': ref('Path')}),
    'ObjectiveCreateCommand': obj({'workspace_id': ID, 'title': ref('Title'), 'intent': ref('Intent'),
                                   'integration_target': ref('Branch'), 'start': ref('Start')}),
    'UpdateIntentCommand': obj({**O, 'expected_intent_revision': COUNTER, 'intent': ref('Intent')}),
    'ExecutionStartCommand': obj({**O, 'model': ref('Model')}),
    'MessageSendCommand': obj({**O, 'text': string(min_length=1), 'mode': ref('DeliveryMode')}),
    'DraftSaveCommand': obj({'draft_id': ID, **O, 'expected_revision': COUNTER, 'reference': ref('Reference'),
                             'body': string()}),
    'DraftDeleteCommand': obj({'draft_id': ID, 'expected_revision': COUNTER}),
    'FeedbackSubmitCommand': obj({**O, 'destination_session_id': ID, 'association_revision': COUNTER,
                                  'drafts': array(ref('DraftRevision'), 256, 1), 'mode': ref('DeliveryMode')},
                                 {'note': string()}),
    'CandidateCommand': obj({**O, 'candidate_id': ID}),
    'ReviewVerifyCommand': obj({**O, 'candidate_id': ID, 'command': ref('Argv')}),
    'ReviewApproveCommand': obj({**O, 'candidate_id': ID, 'target_commit': ref('GitId')},
                                {'override_reason': string(min_length=1, max_length=2048)}),
    'ReviewIntegrateCommand': obj({**O, 'approval_id': ID}),
    'ObjectiveCleanupCommand': obj({**O, 'confirm_data_loss': BOOL}),
    # Deliberate, user-confirmed deletion of a cleaned-up Objective's retention refs and branch; never automatic.
    'ObjectiveHistoryDeleteCommand': obj({**O, 'confirm_history_loss': BOOL}),
    'SettingsUpdateCommand': obj({}, {'default_model': ref('Model'),
                                      'verification_commands': array(ref('Argv'), 32)}),
    # Reply data.
    'SnapshotData': obj({'service_epoch': ID, 'sequence': COUNTER, 'workspaces': array(ref('Workspace'), 64),
                         'objectives': array(ref('ObjectiveSummary'), 512), 'settings': ref('Settings')}),
    'ActivityPage': obj({'items': array(ref('ActivityItem'), 200), 'has_more': BOOL}),
    'ChangeList': obj({'basis_label': ref('Label'), 'target': ref('TargetRef'),
                       'files': array(ref('ChangedFile'), 1024), 'truncated': BOOL,
                       'total_added': COUNTER, 'total_removed': COUNTER},
                      {'capture_id': ID, 'candidate_id': ID, 'notices': array(ref('ContentNotice'), 64)}),
    'FileContent': obj({'artifact_digest': ref('Digest'), 'total_bytes': COUNTER, 'offset': COUNTER,
                        'length': COUNTER, 'encoding': enum('utf8', 'binary'), 'eof': BOOL,
                        'version_label': ref('Label')},
                       # chunked: the file exceeds the whole-file inspection bound and is served in bounded chunks
                       # (encoding judged per chunk); the artifact still holds the whole file.
                       {'text': string(), 'chunked': BOOL}),
    'FileDiff': obj({'binary': BOOL, 'hunks': array(ref('DiffHunk'), 512), 'truncated': BOOL},
                    {'old_digest': ref('Digest'), 'new_digest': ref('Digest')}),
    'SearchResults': obj({'scope_label': ref('Label'), 'results': array(ref('SearchResult'), 200),
                          'truncated': BOOL}),
    'DraftList': obj({'drafts': array(ref('Draft'), 512)}),
    'InputList': obj({'inputs': array(ref('InputSummary'), 512)}),
    'OperationStatus': obj({'state': ref('OperationState')}, {'outcome': ref('Outcome')}),
    # Event payloads.
    'ActivityAppended': obj({**O, 'items': array(ref('ActivityItem'), 200)}),
    'ActivityDelta': obj({**O, 'item_id': ID, 'append_text': string()}),
    'WorktreeChanged': obj({**O, 'capture_id': ID}),
    'ServiceNotice': obj({'level': enum('info', 'warning', 'error'), 'code': string(min_length=1, max_length=64),
                          'message': ref('ShortText')}),
    'Scope': obj({'workspace_id': ID, 'objective_id': ID, 'session_id': ID, 'association_revision': COUNTER}),
    # Contained terminal (IMP-012). Terminal requests are ephemeral interaction with a service-owned PTY inside the
    # Objective sandbox: they create no durable state, are never journaled or replayed, and so carry no
    # operation_id. Sizes are decimal strings like every other counter; the service bounds them.
    'TerminalMode': enum('read_write', 'read_only'),
    'TerminalOpenCommand': obj({**O, 'cols': COUNTER, 'rows': COUNTER}),
    'TerminalInfo': obj({'terminal_id': ID, **O, 'mode': ref('TerminalMode'), 'cols': COUNTER, 'rows': COUNTER}),
    'TerminalInputCommand': obj({'terminal_id': ID, 'data': string(min_length=1, max_length=16384)}),
    'TerminalResizeCommand': obj({'terminal_id': ID, 'cols': COUNTER, 'rows': COUNTER}),
    'TerminalSelector': obj({'terminal_id': ID}),
    # Output is UTF-8 text decoded by the service (invalid bytes replaced); dropped_bytes counts output discarded
    # since the previous event because the bounded buffer overflowed (flood), so the client can mark the gap.
    'TerminalOutput': obj({**O, 'terminal_id': ID, 'seq': COUNTER, 'data': string(max_length=65536),
                           'dropped_bytes': COUNTER}),
    'TerminalClosed': obj({**O, 'terminal_id': ID,
                           'reason': enum('exited', 'closed', 'review_freeze', 'lifecycle', 'failed')},
                          {'exit_code': COUNTER, 'signal': COUNTER}),
    # Explicit Exit / session logout: the service stops its workers and terminals and exits. Nothing resumes on
    # the next start (restart recovery marks interrupted work).
    'ServiceStopCommand': obj({'reason': enum('exit', 'logout')}),
})

# name, mutating, payload def, reply data def. Mutating names require operation_id; queries (and the ephemeral
# terminal.* interaction) must omit it.
REQUESTS = [
    ('snapshot', False, 'Empty', 'SnapshotData'),
    ('objective.get', False, 'ObjectiveSelector', 'ObjectiveDetail'),
    ('activity.page', False, 'ActivityPageQuery', 'ActivityPage'),
    ('changes.list', False, 'ChangesListQuery', 'ChangeList'),
    ('file.read', False, 'FileReadQuery', 'FileContent'),
    ('diff.get', False, 'DiffGetQuery', 'FileDiff'),
    ('search', False, 'SearchQuery', 'SearchResults'),
    ('drafts.list', False, 'ObjectiveSelector', 'DraftList'),
    ('inputs.list', False, 'ObjectiveSelector', 'InputList'),
    ('operation.get', False, 'OperationGetQuery', 'OperationStatus'),
    ('workspace.open', True, 'WorkspaceOpenCommand', 'Workspace'),
    ('objective.create', True, 'ObjectiveCreateCommand', 'ObjectiveDetail'),
    ('objective.update_intent', True, 'UpdateIntentCommand', 'ObjectiveDetail'),
    ('execution.start', True, 'ExecutionStartCommand', 'ObjectiveDetail'),
    ('execution.cancel', True, 'ObjectiveSelector', 'ObjectiveDetail'),
    ('execution.resume', True, 'ObjectiveSelector', 'ObjectiveDetail'),
    ('message.send', True, 'MessageSendCommand', 'InputSummary'),
    ('draft.save', True, 'DraftSaveCommand', 'Draft'),
    ('draft.delete', True, 'DraftDeleteCommand', 'Empty'),
    ('feedback.submit', True, 'FeedbackSubmitCommand', 'InputSummary'),
    ('review.freeze', True, 'ObjectiveSelector', 'CandidateSummary'),
    ('review.return', True, 'CandidateCommand', 'ObjectiveDetail'),
    ('review.verify', True, 'ReviewVerifyCommand', 'VerificationSummary'),
    ('review.approve', True, 'ReviewApproveCommand', 'ApprovalSummary'),
    ('review.refresh', True, 'CandidateCommand', 'CandidateSummary'),
    ('review.integrate', True, 'ReviewIntegrateCommand', 'ObjectiveDetail'),
    ('objective.complete', True, 'ObjectiveSelector', 'ObjectiveDetail'),
    ('objective.discard', True, 'ObjectiveSelector', 'ObjectiveDetail'),
    ('objective.cleanup', True, 'ObjectiveCleanupCommand', 'ObjectiveDetail'),
    ('settings.update', True, 'SettingsUpdateCommand', 'Settings'),
    ('terminal.open', False, 'TerminalOpenCommand', 'TerminalInfo'),
    ('terminal.input', False, 'TerminalInputCommand', 'Empty'),
    ('terminal.resize', False, 'TerminalResizeCommand', 'Empty'),
    ('terminal.close', False, 'TerminalSelector', 'Empty'),
    ('service.stop', True, 'ServiceStopCommand', 'Empty'),
    ('objective.delete_history', True, 'ObjectiveHistoryDeleteCommand', 'ObjectiveDetail'),
]
EVENTS = [
    ('objective.changed', 'ObjectiveSummary'),
    ('objective.detail', 'ObjectiveDetail'),
    ('activity.appended', 'ActivityAppended'),
    ('activity.delta', 'ActivityDelta'),
    ('draft.changed', 'Draft'),
    ('input.changed', 'InputSummary'),
    ('worktree.changed', 'WorktreeChanged'),
    ('verification.changed', 'VerificationSummary'),
    ('service.notice', 'ServiceNotice'),
    ('terminal.output', 'TerminalOutput'),
    ('terminal.closed', 'TerminalClosed'),
]
assert set(client['ActionName']['enum']) <= {n for n, m, _, _ in REQUESTS if m}


def result(data):
    return one_of(obj({'status': const('ok'), 'data': ref(data)}), ref('Rejection'))


client['Request'] = one_of(*[
    obj({'kind': const('request'), 'request_id': ID, **({'operation_id': ID} if mutating else {}),
         'name': const(name), 'payload': ref(payload)})
    for name, mutating, payload, _ in REQUESTS])
# A reply names its request so the data shape is schema-checked; a request whose name could not be read is
# answered by the unnamed rejection.
client['Reply'] = one_of(*[
    obj({'kind': const('reply'), 'request_id': ID, 'name': const(name), 'result': result(data)})
    for name, _, _, data in REQUESTS], obj({'kind': const('reply'), 'request_id': ID, 'result': ref('Rejection')}))
client['Outcome'] = one_of(*[obj({'name': const(name), 'result': result(data)})
                             for name, mutating, _, data in REQUESTS if mutating])
client['Event'] = one_of(*[
    obj({'kind': const('event'), 'service_epoch': ID, 'sequence': COUNTER, 'name': const(name),
         'payload': ref(payload)}) for name, payload in EVENTS])
client['Body'] = one_of(
    obj({'kind': const('hello')}),
    obj({'kind': const('auth'), 'capability': ref('Capability')}),
    obj({'kind': const('welcome'), 'service_epoch': ID, 'lease_id': ID}),
    ref('Request'), ref('Reply'), ref('Event'),
    obj({'kind': const('chunk.start'), 'transfer_id': ID, 'owner': ref('Scope'), 'execution_generation': COUNTER,
         'total_bytes': COUNTER, 'sha256': ref('Digest')}),
    obj({'kind': const('chunk.data'), 'transfer_id': ID, 'index': COUNTER, 'hex': string('chunk-hex')}),
    obj({'kind': const('chunk.end'), 'transfer_id': ID}),
    obj({'kind': const('chunk.cancel'), 'transfer_id': ID}),
)

SANDBOX_PATH = string(min_length=1, max_length=4096)
worker = dict(shared)
worker.update({
    'WorkerErrorCode': enum('startup_failed', 'session_failed', 'provider_failed', 'delivery_failed',
                            'protocol_violation', 'limit_reached', 'internal'),
    'ServiceToWorker': one_of(
        obj({'kind': const('worker.start'), 'objective_id': ID, 'session_dir': SANDBOX_PATH, 'cwd': SANDBOX_PATH,
             'model': ref('Model'), 'resume': BOOL, 'system_note': string()}),
        obj({'kind': const('input.deliver'), 'input_id': ID, 'mode': ref('DeliveryMode'),
             'text': string(min_length=1)}),
        obj({'kind': const('session.abort')}),
        obj({'kind': const('worker.shutdown')}),
    ),
    'WorkerToService': one_of(
        obj({'kind': const('worker.ready'), 'pi_session_id': string(min_length=1, max_length=256),
             'session_file': SANDBOX_PATH}),
        obj({'kind': const('input.received'), 'input_id': ID}),
        obj({'kind': const('activity.item'), 'item_id': ID, 'role': ref('ActivityRole'),
             'title': string(max_length=1024), 'text': string(), 'is_error': BOOL, 'truncated': BOOL},
            {'tool_name': string(max_length=256)}),
        obj({'kind': const('activity.delta'), 'item_id': ID, 'append_text': string()}),
        obj({'kind': const('turn.state'), 'state': enum('running', 'idle')}),
        obj({'kind': const('worker.error'), 'code': ref('WorkerErrorCode'), 'message': string(max_length=512)}),
    ),
    'WorkerBody': one_of(ref('ServiceToWorker'), ref('WorkerToService')),
})


def envelope(schema_id, body, defs):
    return {
        '$schema': 'https://json-schema.org/draft/2020-12/schema',
        '$id': schema_id,
        '$comment': 'Generated by scripts/generate-schema.py; do not edit.',
        'type': 'object', 'additionalProperties': False, 'required': ['protocol', 'bundle', 'body'],
        'properties': {'protocol': const(PROTOCOL), 'bundle': const(BUNDLE), 'body': ref(body)},
        '$defs': defs,
    }


KEYWORDS = {'$schema', '$id', '$comment', '$defs', '$ref', 'oneOf', 'const', 'enum', 'type', 'additionalProperties',
            'required', 'properties', 'items', 'minItems', 'maxItems', 'format', 'minLength', 'maxLength'}
FORMATS = {'uuid-v4', 'u64', 'sha256', 'chunk-hex', 'git-sha1', 'capability', 'timestamp', 'path-b64', 'page-limit',
           'read-limit'}


def check(s, defs, top=False):
    """Fail closed on any construct outside the subset the validators implement."""
    assert set(s) <= KEYWORDS, set(s) - KEYWORDS
    if '$ref' in s:
        assert set(s) == {'$ref'} and s['$ref'].removeprefix('#/$defs/') in defs, s
    elif 'oneOf' in s:
        assert set(s) == {'oneOf'}
        for choice in s['oneOf']:
            check(choice, defs)
    elif 'const' in s or 'enum' in s:
        assert len(s) == 1
    elif s['type'] == 'object':
        keys = {'type', 'additionalProperties', 'required', 'properties'}
        assert set(s) - {'$schema', '$id', '$comment', '$defs'} == keys if top else set(s) == keys, s
        assert s['additionalProperties'] is False and set(s['required']) <= set(s['properties'])
        for p in s['properties'].values():
            check(p, defs)
    elif s['type'] == 'array':
        assert set(s) <= {'type', 'items', 'minItems', 'maxItems'} and 'maxItems' in s and s['maxItems'] <= 1024
        check(s['items'], defs)
    elif s['type'] == 'string':
        assert set(s) <= {'type', 'format', 'minLength', 'maxLength'} and s.get('format', 'uuid-v4') in FORMATS
    else:
        assert s == BOOL, s


def dump(value, indent=0):
    """Stable, review-friendly JSON: small nodes stay on one line."""
    flat = json.dumps(value, ensure_ascii=False)
    if len(flat) + indent <= 118 or not isinstance(value, (dict, list)) or not value:
        return flat
    pad = ' ' * (indent + 2)
    if isinstance(value, list):
        return '[\n' + ',\n'.join(pad + dump(v, indent + 2) for v in value) + '\n' + ' ' * indent + ']'
    return '{\n' + ',\n'.join(f'{pad}{json.dumps(k)}: {dump(v, indent + 2)}' for k, v in value.items()) + '\n' + \
        ' ' * indent + '}'


outputs = {
    'contracts/v1/schema.json': envelope('https://lade.local/contracts/v1', 'Body', client),
    'contracts/v1/worker-schema.json': envelope('https://lade.local/contracts/v1/worker', 'WorkerBody', worker),
}
drift = []
for path, schema in outputs.items():
    check(schema, schema['$defs'], top=True)
    for definition in schema['$defs'].values():
        check(definition, schema['$defs'])
    text = dump(schema) + '\n'
    target = root / path
    if '--check' in sys.argv:
        if not target.exists() or target.read_text() != text:
            drift.append(path)
    else:
        target.write_text(text)
if drift:
    raise SystemExit(f'schemas differ ({", ".join(drift)}); run scripts/generate-schema.py')
