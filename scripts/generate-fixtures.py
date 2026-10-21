#!/usr/bin/env python3
"""Retained deterministic wire corpus; expectations are explicit, not codec-derived.

Valid examples are built from the checked-in schemas (full = every optional field and the last
alternative of each choice; minimal = required fields and the first alternative). Each derived
invalid case states the exact rejection both codecs must produce.
"""
import base64
import copy
import hashlib
import json
from pathlib import Path
import sys

root = Path(__file__).resolve().parent.parent
schemas = {name: json.loads((root / f'contracts/v1/{file}').read_text())
           for name, file in [('client', 'schema.json'), ('worker', 'worker-schema.json')]}
BUNDLE = schemas['client']['properties']['bundle']['const']
assert BUNDLE == schemas['worker']['properties']['bundle']['const']
uid = lambda n: f'00000000-0000-4000-8000-{n:012x}'
scope = dict(workspace_id=uid(1), objective_id=uid(2), session_id=uid(3), association_revision='3')
PATH_BYTES = b'src/\xff\xfename\n.rs'
SAMPLES = {
    'uuid-v4': None, 'u64': '7', 'page-limit': '50', 'read-limit': '65536', 'sha256': 'ab' * 32,
    'capability': '0123456789abcdef' * 4, 'git-sha1': '1234567890' * 4, 'chunk-hex': '61',
    'timestamp': '2026-10-10T12:34:56.789Z', 'path-b64': base64.b64encode(PATH_BYTES).decode(),
}
ids = iter(range(100, 1 << 40))


def envelope(body):
    return dict(protocol=1, bundle=BUNDLE, body=body)


def sample(s, contract, full):
    defs = schemas[contract]['$defs']
    if '$ref' in s:
        return sample(defs[s['$ref'].split('/')[-1]], contract, full)
    if 'oneOf' in s:
        return sample(s['oneOf'][-1 if full else 0], contract, full)
    if 'const' in s:
        return s['const']
    if 'enum' in s:
        return s['enum'][-1 if full else 0]
    t = s['type']
    if t == 'boolean':
        return full
    if t == 'array':
        return [sample(s['items'], contract, full)] if full or s.get('minItems') else []
    if t == 'object':
        return {k: sample(v, contract, full) for k, v in s['properties'].items() if full or k in s['required']}
    fmt = s.get('format')
    if fmt == 'uuid-v4':
        return uid(next(ids))
    if fmt:
        return SAMPLES[fmt]
    text = 'Text á😀 \n"' if full else 'text'
    return text[:s.get('maxLength', len(text))]


def variants(contract, name):
    return schemas[contract]['$defs'][name]['oneOf']


def const_of(variant, key):
    return variant['properties'][key]['const']


corpus = []


def case(name, mode, source, expected='ok', **extra):
    assert all(c['id'] != name for c in corpus), name
    corpus.append(dict(id=name, mode=mode, source=source, expected=expected, **extra))


def val(v):
    return dict(value=v)


def frame(name, body, expected='ok', contract='client'):
    case(name, 'frame', val(envelope(body)), expected, contract=contract)


def edit(value, path, replacement):
    v = copy.deepcopy(value)
    p = v
    for k in path[:-1]:
        p = p[k]
    if replacement is DELETE:
        del p[path[-1]]
    else:
        p[path[-1]] = replacement
    return v


DELETE = object()

frame('hello', dict(kind='hello'))
frame('auth', dict(kind='auth', capability=SAMPLES['capability']))
frame('welcome', dict(kind='welcome', service_epoch=uid(6), lease_id=uid(7)))
frame('auth-uppercase', dict(kind='auth', capability=SAMPLES['capability'].upper()), 'invalid_schema')
frame('auth-short', dict(kind='auth', capability=SAMPLES['capability'][:-1]), 'invalid_schema')
frame('welcome-missing-lease', dict(kind='welcome', service_epoch=uid(6)), 'invalid_schema')

requests = {}
for variant in variants('client', 'Request'):
    name = const_of(variant, 'name')
    mutating = 'operation_id' in variant['properties']
    payload_def = schemas['client']['$defs'][variant['properties']['payload']['$ref'].split('/')[-1]]
    full = sample(variant, 'client', True)
    minimal = sample(variant, 'client', False)
    requests[name] = full
    frame(f'request-{name}-full', full)
    frame(f'request-{name}-minimal', minimal)
    frame(f'request-{name}-unknown-payload-field', edit(minimal, ['payload', 'unexpected'], 'x'), 'invalid_schema')
    frame(f'request-{name}-payload-array', edit(minimal, ['payload'], []), 'invalid_schema')
    if mutating:
        frame(f'request-{name}-missing-operation-id', edit(minimal, ['operation_id'], DELETE), 'invalid_schema')
    else:
        frame(f'request-{name}-forbidden-operation-id', edit(minimal, ['operation_id'], uid(5)), 'invalid_schema')
    if payload_def['required']:
        first = payload_def['required'][0]
        frame(f'request-{name}-missing-{first}', edit(minimal, ['payload', first], DELETE), 'invalid_schema')
    optional = [k for k in payload_def['properties'] if k not in payload_def['required']]
    if optional:
        frame(f'request-{name}-null-{optional[0]}', edit(full, ['payload', optional[0]], None), 'invalid_schema')

for variant in variants('client', 'Reply'):
    if 'name' not in variant['properties']:
        continue
    name = const_of(variant, 'name')
    data = variant['properties']['result']['oneOf'][0]['properties']['data']
    head = dict(kind='reply', request_id=uid(4), name=name)
    frame(f'reply-{name}-ok-full', dict(head, result=dict(status='ok', data=sample(data, 'client', True))))
    frame(f'reply-{name}-ok-minimal', dict(head, result=dict(status='ok', data=sample(data, 'client', False))))
    frame(f'reply-{name}-rejected', dict(head, result=sample({'$ref': '#/$defs/Rejection'}, 'client', False)))
    frame(f'reply-{name}-unknown-data-field',
          dict(head, result=dict(status='ok', data=dict(sample(data, 'client', False), unexpected=True))),
          'invalid_schema')
rejection = dict(status='rejected', code='invalid_request', action='none', detail='unknown request name')
frame('reply-unnamed-rejected', dict(kind='reply', request_id=uid(4), result=rejection))
frame('reply-unnamed-ok', dict(kind='reply', request_id=uid(4), result=dict(status='ok', data={})), 'invalid_schema')
frame('reply-unknown-name', dict(kind='reply', request_id=uid(4), name='shell.execute', result=rejection),
      'invalid_schema')
detail = dict(kind='reply', request_id=uid(4), name='snapshot', result=rejection)
for label, text, expected in [('512-ascii', 'a' * 512, 'ok'), ('512-astral', '😀' * 512, 'ok'),
                              ('513-ascii', 'a' * 513, 'invalid_schema'), ('513-astral', '😀' * 513, 'invalid_schema')]:
    frame(f'reply-detail-{label}', edit(detail, ['result', 'detail'], text), expected)
frame('reply-mismatched-data', dict(kind='reply', request_id=uid(4), name='review.freeze',
                                    result=dict(status='ok', data=requests['objective.get']['payload'])),
      'invalid_schema')
for error in schemas['client']['$defs']['ErrorCode']['enum']:
    frame('error-' + error, edit(detail, ['result', 'code'], error))
for action in schemas['client']['$defs']['SafeAction']['enum']:
    frame('action-' + action, edit(detail, ['result', 'action'], action))
status = dict(kind='reply', request_id=uid(4), name='operation.get')
send_data = sample({'$ref': '#/$defs/InputSummary'}, 'client', False)
frame('operation-outcome-ok', dict(status, result=dict(status='ok', data=dict(
    state='completed', outcome=dict(name='message.send', result=dict(status='ok', data=send_data))))))
frame('operation-outcome-query-name', dict(status, result=dict(status='ok', data=dict(
    state='completed', outcome=dict(name='objective.get', result=rejection)))), 'invalid_schema')
frame('operation-outcome-wrong-shape', dict(status, result=dict(status='ok', data=dict(
    state='completed', outcome=dict(name='draft.delete', result=dict(status='ok', data=send_data))))),
      'invalid_schema')

for variant in variants('client', 'Event'):
    name = const_of(variant, 'name')
    frame(f'event-{name}-full', sample(variant, 'client', True))
    minimal = sample(variant, 'client', False)
    frame(f'event-{name}-minimal', minimal)
    frame(f'event-{name}-unknown-field', edit(minimal, ['payload', 'unexpected'], 1), 'invalid_schema')
changed = sample(variants('client', 'Event')[0], 'client', False)
frame('event-u64-max', edit(changed, ['sequence'], '18446744073709551615'))
frame('event-u64-overflow', edit(changed, ['sequence'], '18446744073709551616'), 'invalid_schema')
for label, value, expected in [
        ('epoch', '1970-01-01T00:00:00.000Z', 'ok'), ('leap-day', '2028-02-29T23:59:59.999Z', 'ok'),
        ('max', '9999-12-31T23:59:59.999Z', 'ok'), ('not-leap', '2026-02-29T00:00:00.000Z', 'invalid_schema'),
        ('century-not-leap', '2100-02-29T00:00:00.000Z', 'invalid_schema'),
        ('hour-24', '2026-10-10T24:00:00.000Z', 'invalid_schema'),
        ('leap-second', '2026-12-31T23:59:60.000Z', 'invalid_schema'),
        ('no-millis', '2026-10-10T12:34:56Z', 'invalid_schema'),
        ('micros', '2026-10-10T12:34:56.789000Z', 'invalid_schema'),
        ('lowercase-z', '2026-10-10T12:34:56.789z', 'invalid_schema'),
        ('offset', '2026-10-10T12:34:56.789+00:00', 'invalid_schema'),
        ('space', '2026-10-10 12:34:56.789Z', 'invalid_schema'),
        ('pre-epoch', '1969-12-31T23:59:59.999Z', 'invalid_schema'),
        ('month-13', '2026-13-01T00:00:00.000Z', 'invalid_schema'),
        ('day-zero', '2026-10-00T00:00:00.000Z', 'invalid_schema'),
        ('fullwidth-digit', '２026-10-10T12:34:56.789Z', 'invalid_schema'),
        ('number', 1791635696789, 'invalid_schema')]:
    frame(f'timestamp-{label}', edit(changed, ['payload', 'updated_at'], value), expected)

open_request = requests['workspace.open']
for label, value, expected in [
        ('raw-ff', '/w==', 'ok'), ('two-pad', 'Zg==', 'ok'), ('one-pad', 'Zm8=', 'ok'),
        ('missing-pad', 'Zm8', 'invalid_schema'), ('nonzero-pad-bits', 'Zh==', 'invalid_schema'),
        ('nonzero-pad-bits-one', 'Zm9=', 'invalid_schema'), ('url-alphabet', '-_-_', 'invalid_schema'),
        ('newline', 'Zm9v\n', 'invalid_schema'), ('empty', '', 'invalid_schema'),
        ('inner-pad', 'Zg==Zg==', 'invalid_schema'), ('at-limit', base64.b64encode(b'a' * 4096).decode(), 'ok'),
        ('over-limit', base64.b64encode(b'a' * 4097).decode(), 'invalid_schema')]:
    frame(f'path-b64-{label}', edit(open_request, ['payload', 'path', 'b64'], value), expected)
for label, value, expected in [('4096', '😀' * 4096, 'ok'), ('4097', '😀' * 4097, 'invalid_schema')]:
    frame(f'path-display-{label}', edit(open_request, ['payload', 'path', 'display'], value), expected)

approve = requests['review.approve']
for label, commit, expected in [
        ('sha1', dict(algorithm='sha1', hex='a' * 40), 'ok'), ('sha256', dict(algorithm='sha256', hex='b' * 64), 'ok'),
        ('sha1-long', dict(algorithm='sha1', hex='a' * 64), 'invalid_schema'),
        ('sha256-short', dict(algorithm='sha256', hex='b' * 40), 'invalid_schema'),
        ('uppercase', dict(algorithm='sha1', hex='A' * 40), 'invalid_schema'),
        ('unknown-algorithm', dict(algorithm='md5', hex='a' * 32), 'invalid_schema'),
        ('bare-hex', 'a' * 40, 'invalid_schema'),
        ('extra-field', dict(algorithm='sha1', hex='a' * 40, short='aaaaaaa'), 'invalid_schema')]:
    frame(f'git-id-{label}', edit(approve, ['payload', 'target_commit'], commit), expected)
frame('approve-empty-override', edit(approve, ['payload', 'override_reason'], ''), 'invalid_schema')

page = requests['activity.page']
for value, expected in [('1', 'ok'), ('200', 'ok'), ('0', 'invalid_schema'), ('201', 'invalid_schema'),
                        ('050', 'invalid_schema'), (50, 'invalid_schema')]:
    frame(f'page-limit-{value!r}', edit(page, ['payload', 'limit'], value), expected)
draft_query = requests['drafts.list']
draft_cursor = dict(created_at='2026-10-10T12:34:56.789Z', draft_id=uid(30))
frame('drafts-page-first', edit(draft_query, ['payload', 'after'], DELETE))
frame('drafts-page-after', edit(draft_query, ['payload', 'after'], draft_cursor))
for label, cursor in [
        ('missing-time', dict(draft_id=uid(30))),
        ('invalid-time', dict(draft_cursor, created_at='2026-02-29T00:00:00.000Z')),
        ('invalid-id', dict(draft_cursor, draft_id='00000000-0000-0000-0000-000000000000')),
        ('extra-field', dict(draft_cursor, offset='1')),
        ('null', None), ('counter', '1')]:
    frame(f'drafts-page-cursor-{label}', edit(draft_query, ['payload', 'after'], cursor), 'invalid_schema')
input_query = requests['inputs.list']
for label, cursor, expected in [
        ('zero', '0', 'ok'), ('max', '18446744073709551615', 'ok'),
        ('leading-zero', '01', 'invalid_schema'), ('overflow', '18446744073709551616', 'invalid_schema'),
        ('negative', '-1', 'invalid_schema'), ('number', 1, 'invalid_schema'),
        ('draft-cursor', draft_cursor, 'invalid_schema')]:
    frame(f'inputs-page-cursor-{label}', edit(input_query, ['payload', 'after'], cursor), expected)
for name, field, definition in [('drafts.list', 'drafts', 'Draft'), ('inputs.list', 'inputs', 'InputSummary')]:
    entry = sample({'$ref': '#/$defs/' + definition}, 'client', False)
    for count, expected in [(200, 'ok'), (201, 'invalid_schema')]:
        frame(f'{name}-page-count-{count}', dict(kind='reply', request_id=uid(4), name=name,
              result=dict(status='ok', data={field: [entry] * count})), expected)
read = requests['file.read']
for value, expected in [('262144', 'ok'), ('262145', 'invalid_schema'), ('524288', 'invalid_schema')]:
    frame(f'read-limit-{value}', edit(read, ['payload', 'limit'], value), expected)
diff_line = dict(kind='add', text='line\r', no_newline=False, new_no='1', new_byte_start='0')
diff_reply = dict(kind='reply', request_id=uid(4), name='diff.get', result=dict(status='ok', data=dict(
    binary=False, hunks=[dict(old_start='0', old_lines='0', new_start='1', new_lines='1', lines=[diff_line])],
    truncated=False)))
for label, text, no_newline in [('crlf', 'line\r', False), ('final-bare-cr', 'line\r', True),
                                ('final-without-newline', 'line', True)]:
    line = dict(diff_line, text=text, no_newline=no_newline)
    frame(f'diff-line-{label}', edit(diff_reply, ['result', 'data', 'hunks', 0, 'lines', 0], line))
for label, value in [('missing', DELETE), ('string', 'true'), ('number', 1), ('null', None)]:
    frame(f'diff-line-no-newline-{label}', edit(diff_reply,
          ['result', 'data', 'hunks', 0, 'lines', 0, 'no_newline'], value), 'invalid_schema')
changes = requests['changes.list']
frame('basis-candidate', edit(changes, ['payload', 'basis'], dict(kind='candidate', candidate_id=uid(30))))
frame('basis-since-incomplete', edit(changes, ['payload', 'basis'], dict(kind='since', from_candidate_id=uid(30))),
      'invalid_schema')
frame('basis-unknown', edit(changes, ['payload', 'basis'], dict(kind='head')), 'invalid_schema')
create = requests['objective.create']
frame('start-target', edit(create, ['payload', 'start'], dict(kind='target')))
frame('start-bare-hex', edit(create, ['payload', 'start'], 'a' * 40), 'invalid_schema')
for label, value, expected in [('empty', '', 'invalid_schema'), ('200', 'é' * 200, 'ok'),
                               ('201', 'é' * 201, 'invalid_schema')]:
    frame(f'title-{label}', edit(create, ['payload', 'title'], value), expected)
frame('branch-empty', edit(create, ['payload', 'integration_target'], ''), 'invalid_schema')
verify = requests['review.verify']
frame('argv-empty', edit(verify, ['payload', 'command'], []), 'invalid_schema')
frame('argv-empty-element', edit(verify, ['payload', 'command'], ['git', '']))
frame('argv-not-string', edit(verify, ['payload', 'command'], ['git', 1]), 'invalid_schema')
submit = requests['feedback.submit']
revision = dict(draft_id=uid(31), revision='2')
for label, n, expected in [('256', 256, 'ok'), ('257', 257, 'invalid_schema'), ('0', 0, 'invalid_schema')]:
    frame(f'drafts-{label}', edit(submit, ['payload', 'drafts'], [revision] * n), expected)
cleanup = requests['objective.cleanup']
frame('boolean-as-string', edit(cleanup, ['payload', 'confirm_data_loss'], 'true'), 'invalid_schema')
frame('boolean-as-number', edit(cleanup, ['payload', 'confirm_data_loss'], 1), 'invalid_schema')
frame('message-empty-text', edit(requests['message.send'], ['payload', 'text'], ''), 'invalid_schema')
frame('message-unknown-mode', edit(requests['message.send'], ['payload', 'mode'], 'steering'), 'invalid_schema')

command = requests['draft.delete']
for name, path, replacement, error in [
        ('wrong-protocol', ['protocol'], 2, 'version_mismatch'), ('wrong-bundle', ['bundle'], '0.1.0-imp001',
                                                                     'version_mismatch'),
        ('missing-bundle', ['bundle'], DELETE, 'version_mismatch'),
        ('envelope-extra-field', ['trace'], 'x', 'invalid_schema'),
        ('role-forgery', ['body', 'role'], 'client', 'invalid_schema'),
        ('approval-forgery', ['body', 'approved'], True, 'invalid_schema'),
        ('bad-uuid', ['body', 'request_id'], 'request-1', 'invalid_schema'),
        ('nil-uuid', ['body', 'request_id'], '00000000-0000-0000-0000-000000000000', 'invalid_schema'),
        ('uuid-uppercase', ['body', 'request_id'], 'ABCDEF00-0000-4000-8000-000000000000', 'invalid_schema'),
        ('uuid-v1', ['body', 'operation_id'], '00000000-0000-1000-8000-000000000000', 'invalid_schema'),
        ('numeric-revision', ['body', 'payload', 'expected_revision'], 3, 'invalid_schema'),
        ('leading-zero', ['body', 'payload', 'expected_revision'], '03', 'invalid_schema'),
        ('overflow-revision', ['body', 'payload', 'expected_revision'], '18446744073709551616', 'invalid_schema'),
        ('negative-revision', ['body', 'payload', 'expected_revision'], '-1', 'invalid_schema'),
        ('null-revision', ['body', 'payload', 'expected_revision'], None, 'invalid_schema'),
        ('unknown-command', ['body', 'name'], 'shell.execute', 'invalid_schema'),
        ('cross-name-payload', ['body', 'name'], 'review.freeze', 'invalid_schema')]:
    case(name, 'frame', val(edit(envelope(command), path, replacement)), error, contract='client')
case('duplicate-key', 'frame',
     dict(text='{"protocol":1,"protocol":1,"bundle":"' + BUNDLE + '","body":{"kind":"hello"}}'), 'duplicate_key',
     contract='client')
case('escaped-duplicate-key', 'json', dict(text='{"a":1,"\\u0061":2}'), 'duplicate_key')
for name, text, expected in [
        ('unicode', '"á\\r\\n\\t漢字😀é  "', 'ok'), ('surrogate-pair', '"\\ud83d\\ude00"', 'ok'),
        ('unpaired-high', '"\\ud800"', 'invalid_json'), ('unpaired-low', '"\\udc00"', 'invalid_json'),
        ('bom', '﻿{}', 'invalid_json'), ('trailing', '{}{}', 'invalid_json'), ('comma', '[1,]', 'invalid_json'),
        ('unsafe-number', '9007199254740992', 'invalid_json'), ('decimal-number', '1.0', 'invalid_json'),
        ('exponent-number', '1e0', 'invalid_json'),
        ('negative-zero', '-0', 'invalid_json'), ('raw-newline', '"x\ny"', 'invalid_json'),
        ('bad-escape', '"\\x"', 'invalid_json')]:
    case(name, 'json', dict(text=text), expected)
case('bad-utf8', 'json', dict(hex='c328'), 'invalid_utf8')
case('overlong-utf8', 'json', dict(hex='c080'), 'invalid_utf8')
for name, recipe, expected in [
        ('depth-limit', dict(depth=32), 'ok'), ('depth-over', dict(depth=33), 'limit_reached'),
        ('array-limit', dict(array=1024), 'ok'), ('array-over', dict(array=1025), 'limit_reached'),
        ('string-limit', dict(string=262144), 'ok'), ('string-over', dict(string=262145), 'limit_reached'),
        ('nodes-over', dict(nodes=True), 'limit_reached'), ('frame-over', dict(padding=1048577), 'frame_size'),
        ('frame-exact', dict(padding=1048576), 'ok')]:
    case(name, 'json', recipe, expected)
full_budget = '[' + ','.join('[' + ','.join(['0'] * n) + ']' for n in [1023] * 15 + [1022]) + ']'
for name, text, expected in [('nodes-exact', full_budget, 'ok'),
                             ('nodes-exact-trailing-comma', full_budget[:-2] + ',]]', 'invalid_json'),
                             ('nodes-exact-dangling-comma', full_budget[:-2] + ',', 'invalid_json')]:
    case(name, 'json', dict(text=text), expected)
hello = envelope(dict(kind='hello'))
abort = envelope(dict(kind='session.abort'))
case('client-header-over', 'transport', dict(hex='00100001'), 'frame_size', framing='client', contract='client')
case('client-header-zero', 'transport', dict(hex='00000000'), 'frame_size', framing='client', contract='client')
case('partial-header', 'transport', dict(hex='0000'), 'truncated_frame', framing='client', contract='client')
case('partial-body', 'transport', dict(hex='000000027b'), 'truncated_frame', framing='client', contract='client')
case('worker-eof', 'transport', dict(text=json.dumps(abort)), 'truncated_frame', framing='worker', contract='worker')
case('worker-empty', 'transport', dict(text='\n'), 'frame_size', framing='worker', contract='worker')
case('worker-over', 'transport', dict(padding=1048577), 'frame_size', framing='worker', contract='worker')
case('worker-unicode-separator', 'transport', dict(text=json.dumps(abort) + ' \n'), 'invalid_json',
     framing='worker', contract='worker')
case('worker-malformed-then-valid', 'transport', dict(text='broken\n' + json.dumps(abort) + '\n'), 'invalid_json',
     framing='worker', contract='worker')
case('worker-client-body-line', 'transport', dict(text=json.dumps(hello) + '\n' + json.dumps(abort) + '\n'),
     'invalid_schema', framing='worker', contract='worker')

for direction in ('ServiceToWorker', 'WorkerToService'):
    for variant in variants('worker', direction):
        kind = const_of(variant, 'kind')
        frame(f'worker-{kind}-full', sample(variant, 'worker', True), contract='worker')
        frame(f'worker-{kind}-minimal', sample(variant, 'worker', False), contract='worker')
        frame(f'worker-{kind}-unknown-field', dict(sample(variant, 'worker', False), unexpected='x'),
              'invalid_schema', contract='worker')
        frame(f'worker-{kind}-on-client-link', sample(variant, 'worker', False), 'invalid_schema')
frame('worker-hello-rejected', dict(kind='hello'), 'invalid_schema', contract='worker')
frame('worker-request-rejected', requests['snapshot'], 'invalid_schema', contract='worker')
deliver = sample(variants('worker', 'ServiceToWorker')[1], 'worker', False)
frame('worker-deliver-empty-text', dict(deliver, text=''), 'invalid_schema', contract='worker')
frame('worker-deliver-bad-mode', dict(deliver, mode='followUp'), 'invalid_schema', contract='worker')
error_body = sample(next(v for v in variants('worker', 'WorkerToService') if const_of(v, 'kind') == 'worker.error'),
                    'worker', False)
frame('worker-error-message-512', dict(error_body, message='😀' * 512), contract='worker')
frame('worker-error-message-513', dict(error_body, message='😀' * 513), 'invalid_schema', contract='worker')
frame('worker-error-unknown-code', dict(error_body, code='crashed'), 'invalid_schema', contract='worker')
item = sample(variants('worker', 'WorkerToService')[2], 'worker', True)
frame('worker-item-null-tool', dict(item, tool_name=None), 'invalid_schema', contract='worker')
frame('worker-item-separators', dict(item, text='a b c\nd'), contract='worker')
case('worker-wrong-bundle', 'frame', val(dict(abort, bundle='0.1.0-imp001')), 'version_mismatch', contract='worker')
case('worker-numeric-protocol-string', 'frame', val(dict(abort, protocol='1')), 'version_mismatch', contract='worker')

payload = 'á\r\n😀  '.encode()
start = dict(kind='chunk.start', transfer_id=uid(9), owner=scope, execution_generation='4', total_bytes=str(len(payload)),
             sha256=hashlib.sha256(payload).hexdigest())
data = dict(kind='chunk.data', transfer_id=uid(9), index='0', hex=payload.hex())
end = dict(kind='chunk.end', transfer_id=uid(9))
cancel = dict(kind='chunk.cancel', transfer_id=uid(9))
for name, body in [('start', start), ('data', data), ('end', end), ('cancel', cancel)]:
    frame('chunk-dto-' + name, body)
for name, replacement in [('object', {}), ('object-array', [{}]), ('array', []), ('null', None), ('boolean', True),
                          ('number', 7)]:
    frame('transfer-id-' + name, dict(end, transfer_id=replacement), 'invalid_schema')
for name, original, path in [
        ('request-id', envelope(command), ['body', 'request_id']),
        ('operation-id', envelope(command), ['body', 'operation_id']),
        ('draft-revision', envelope(command), ['body', 'payload', 'expected_revision']),
        ('sequence', envelope(changed), ['body', 'sequence']),
        ('execution-generation', envelope(start), ['body', 'execution_generation']),
        ('total-bytes', envelope(start), ['body', 'total_bytes']),
        ('digest', envelope(start), ['body', 'sha256']),
        ('chunk-index', envelope(data), ['body', 'index']),
        ('chunk-hex', envelope(data), ['body', 'hex']),
        ('worker-input-id', envelope(deliver), ['body', 'input_id'])]:
    for kind, replacement in [('object', {}), ('object-array', [{}])]:
        case(name + '-' + kind, 'frame', val(edit(original, path, replacement)), 'invalid_schema',
             contract='worker' if name.startswith('worker') else 'client')
for name, replacement in [('string', '0.1.0'), ('object', {}), ('object-array', [{}])]:
    v = envelope(dict(end, transfer_id={}))
    v['bundle'] = replacement
    case('bundle-' + name + '-invalid-transfer', 'frame', val(v), 'version_mismatch', contract='client')
for name, steps, expected in [
        ('chunk-roundtrip', [start, data, end], 'ok'), ('chunk-cancel', [start, cancel], 'ok'),
        ('chunk-unstarted', [data], 'chunk_order'), ('chunk-repeat-start', [start, start], 'chunk_order'),
        ('chunk-after-cancel', [start, cancel, data], 'chunk_order'), ('chunk-early-end', [start, end], 'chunk_length'),
        ('chunk-bad-digest', [dict(start, sha256='0' * 64), data, end], 'chunk_checksum'),
        ('chunk-out-of-order', [start, dict(data, index='1')], 'chunk_order'),
        ('chunk-length-over', [dict(start, total_bytes='1'), data], 'limit_reached'),
        ('chunk-size-over', [dict(start, total_bytes='8388609')], 'limit_reached'),
        ('chunk-empty', [dict(start, total_bytes='0')], 'limit_reached'),
        ('chunk-stale-generation', [dict(start, execution_generation='5')], 'stale_execution'),
        ('chunk-wrong-owner', [dict(start, owner=dict(scope, objective_id=uid(99)))], 'stale_execution'),
        ('chunk-transfer-count', [start, dict(start, transfer_id=uid(10)), dict(start, transfer_id=uid(11))],
         'limit_reached')]:
    case(name, 'chunks', dict(steps=[envelope(s) for s in steps]), expected, owner=scope, generation='4',
         output_hex=payload.hex() if name == 'chunk-roundtrip' else '')
frame('chunk-over-limit', dict(data, hex='aa' * 65537), 'invalid_schema')
frame('chunk-at-limit', dict(data, hex='aa' * 65536))
frame('chunk-invalid-hex', dict(data, hex='AA'), 'invalid_schema')
frame('chunk-odd-hex', dict(data, hex='a'), 'invalid_schema')

original = envelope(requests['message.send'])
repeat = copy.deepcopy(original)
new_request = edit(original, ['body', 'request_id'], uid(20))
changed_payload = edit(new_request, ['body', 'payload', 'text'], 'different text')
changed_operation = edit(original, ['body', 'operation_id'], uid(21))
reordered = copy.deepcopy(new_request)
reordered['body']['payload'] = dict(reversed(list(reordered['body']['payload'].items())))
query = envelope(requests['snapshot'])
for name, steps, expected in [
        ('duplicate-request', [original, repeat], 'ok'), ('duplicate-operation', [original, new_request], 'ok'),
        ('duplicate-reordered-keys', [original, reordered], 'ok'),
        ('operation-conflict', [original, changed_payload], 'duplicate_conflict'),
        ('request-conflict', [original, changed_operation], 'duplicate_conflict'),
        ('history-query-rejected', [query], 'invalid_schema')]:
    case(name, 'history', dict(steps=steps), expected)

output = '[\n' + ',\n'.join(json.dumps(c, ensure_ascii=False) for c in corpus) + '\n]\n'
path = root / 'contracts/v1/fixtures.json'
if '--check' in sys.argv:
    if path.read_text() != output:
        raise SystemExit('fixtures differ; run scripts/generate-fixtures.py')
else:
    path.write_text(output)
