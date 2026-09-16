#!/usr/bin/env python3
"""Retained deterministic wire corpus; expectations are explicit, not codec-derived."""
import copy
import hashlib
import json
from pathlib import Path
import sys

root = Path(__file__).resolve().parent.parent
uid = lambda n: f'00000000-0000-4000-8000-{n:012x}'
scope = dict(workspace_id=uid(1), objective_id=uid(2), session_id=uid(3), association_revision='3')
def envelope(body):
    return dict(protocol=1, bundle='0.1.0-imp001', body=body)
hello = envelope(dict(kind='hello'))
command = envelope(dict(kind='command', request_id=uid(4), operation_id=uid(5), expected_service_epoch=uid(6), scope=scope,
                        command=dict(name='feedback.submit', payload=dict(batch_id=uid(7), expected_draft_revision='7', delivery_mode='follow_up'))))
corpus = []
def case(name, mode, source, expected='ok', **extra):
    corpus.append(dict(id=name, mode=mode, source=source, expected=expected, **extra))
def val(v): return dict(value=v)
case('hello', 'frame', val(hello))
case('submit', 'frame', val(command))
q=copy.deepcopy(command); q['body']['command']=dict(name='operation.get', payload=dict(operation_id=uid(5)))
case('query', 'frame', val(q))
for status, result in [('accepted',dict(input_id=uid(8))), ('operation',dict(state='uncertain')), ('rejected',dict(code='unauthorized',action='none'))]:
    case('response-'+status, 'frame', val(envelope(dict(kind='response',request_id=uid(4),operation_id=uid(5),result=dict(status=status,**result)))))
for error in json.loads((root/'contracts/v1/schema.json').read_text())['$defs']['ErrorCode']['enum']:
    case('error-'+error, 'frame', val(envelope(dict(kind='response',request_id=uid(4),operation_id=uid(5),result=dict(status='rejected',code=error,action='query_operation')))))
event=envelope(dict(kind='event', service_epoch=uid(6),sequence='18446744073709551615',scope=scope,operation_id=uid(5),state='uncertain'))
case('event-u64-max','frame',val(event))
for name,path,replacement,error in [
 ('wrong-protocol',['protocol'],2,'version_mismatch'),('wrong-bundle',['bundle'],'0.1.0','version_mismatch'),
 ('role-forgery',['body','role'],'client','invalid_schema'),('approval-forgery',['body','approved'],True,'invalid_schema'),
 ('bad-uuid',['body','request_id'],'request-1','invalid_schema'),('nil-uuid',['body','request_id'],'00000000-0000-0000-0000-000000000000','invalid_schema'),
 ('uuid-uppercase',['body','request_id'],'ABCDEF00-0000-4000-8000-000000000000','invalid_schema'),
 ('numeric-revision',['body','scope','association_revision'],3,'invalid_schema'),('leading-zero',['body','scope','association_revision'],'03','invalid_schema'),
 ('overflow-revision',['body','scope','association_revision'],'18446744073709551616','invalid_schema'),
 ('negative-revision',['body','scope','association_revision'],'-1','invalid_schema'),
 ('unknown-command',['body','command','name'],'shell.execute','invalid_schema')]:
    v=copy.deepcopy(command); p=v
    for k in path[:-1]: p=p[k]
    p[path[-1]]=replacement
    case(name,'frame',val(v),error)
case('duplicate-key','frame',dict(text='{"protocol":1,"protocol":1,"bundle":"0.1.0-imp001","body":{"kind":"hello"}}'),'duplicate_key')
case('escaped-duplicate-key','json',dict(text='{"a":1,"\\u0061":2}'),'duplicate_key')
for name,text,expected in [
 ('unicode','"á\\r\\n\\t漢字😀é\u2028\u2029"','ok'), ('surrogate-pair','"\\ud83d\\ude00"','ok'),
 ('unpaired-high','"\\ud800"','invalid_json'),('unpaired-low','"\\udc00"','invalid_json'),
 ('bom','\ufeff{}','invalid_json'),('trailing','{}{}','invalid_json'),('comma','[1,]','invalid_json'),
 ('unsafe-number','9007199254740992','invalid_json'),('decimal-number','1.0','invalid_json'),('exponent-number','1e0','invalid_json'),
 ('negative-zero','-0','invalid_json'),('raw-newline','"x\ny"','invalid_json'),('bad-escape','"\\x"','invalid_json')]:
    case(name,'json',dict(text=text),expected)
case('bad-utf8','json',dict(hex='c328'),'invalid_utf8')
case('overlong-utf8','json',dict(hex='c080'),'invalid_utf8')
for name,recipe,expected in [
 ('depth-limit',dict(depth=32),'ok'),('depth-over',dict(depth=33),'limit_reached'),
 ('array-limit',dict(array=1024),'ok'),('array-over',dict(array=1025),'limit_reached'),
 ('string-limit',dict(string=262144),'ok'),('string-over',dict(string=262145),'limit_reached'),
 ('nodes-over',dict(nodes=True),'limit_reached'),('frame-over',dict(padding=1048577),'frame_size'),
 ('frame-exact',dict(padding=1048576),'ok')]:
    case(name,'json',recipe,expected)
case('client-header-over','transport',dict(hex='00100001'),'frame_size',framing='client')
case('client-header-zero','transport',dict(hex='00000000'),'frame_size',framing='client')
case('partial-header','transport',dict(hex='0000'),'truncated_frame',framing='client')
case('partial-body','transport',dict(hex='000000027b'),'truncated_frame',framing='client')
case('worker-eof','transport',dict(text=json.dumps(hello)),'truncated_frame',framing='worker')
case('worker-empty','transport',dict(text='\n'),'frame_size',framing='worker')
case('worker-over','transport',dict(padding=1048577),'frame_size',framing='worker')
# Raw Unicode separators in JSON whitespace are invalid, not line delimiters.
case('worker-unicode-separator','transport',dict(text=json.dumps(hello)+'\u2028\n'),'invalid_json',framing='worker')
case('worker-malformed-then-valid','transport',dict(text='broken\n'+json.dumps(hello)+'\n'),'invalid_json',framing='worker')

payload='á\r\n😀\u2028\u2029'.encode()
start=dict(kind='chunk.start',transfer_id=uid(9),owner=scope,execution_generation='4',total_bytes=str(len(payload)),sha256=hashlib.sha256(payload).hexdigest())
data=dict(kind='chunk.data',transfer_id=uid(9),index='0',hex=payload.hex())
end=dict(kind='chunk.end',transfer_id=uid(9))
cancel=dict(kind='chunk.cancel',transfer_id=uid(9))
for name,body in [('start',start),('data',data),('end',end),('cancel',cancel)]: case('chunk-dto-'+name,'frame',val(envelope(body)))
# Wrong JSON types must reject without coercing null-prototype objects to strings.
for name,replacement in [('object',{}),('object-array',[{}]),('array',[]),('null',None),('boolean',True),('number',7)]:
    case('transfer-id-'+name,'frame',val(envelope(dict(end,transfer_id=replacement))),'invalid_schema')
for name,original,path in [
 ('request-id',command,['body','request_id']),
 ('workspace-id',command,['body','scope','workspace_id']),
 ('association-revision',command,['body','scope','association_revision']),
 ('draft-revision',command,['body','command','payload','expected_draft_revision']),
 ('sequence',event,['body','sequence']),
 ('execution-generation',envelope(start),['body','execution_generation']),
 ('total-bytes',envelope(start),['body','total_bytes']),
 ('digest',envelope(start),['body','sha256']),
 ('chunk-index',envelope(data),['body','index']),
 ('chunk-hex',envelope(data),['body','hex'])]:
    for kind,replacement in [('object',{}),('object-array',[{}])]:
        v=copy.deepcopy(original); p=v
        for k in path[:-1]: p=p[k]
        p[path[-1]]=replacement
        case(name+'-'+kind,'frame',val(v),'invalid_schema')
# Version rejection takes precedence, even if the body also has an invalid type.
for name,replacement in [('string','0.1.0'),('object',{}),('object-array',[{}])]:
    v=envelope(dict(end,transfer_id={})); v['bundle']=replacement
    case('bundle-'+name+'-invalid-transfer','frame',val(v),'version_mismatch')
for name,steps,expected in [
 ('chunk-roundtrip',[start,data,end],'ok'),('chunk-cancel',[start,cancel],'ok'),
 ('chunk-unstarted',[data],'chunk_order'),('chunk-repeat-start',[start,start],'chunk_order'),
 ('chunk-after-cancel',[start,cancel,data],'chunk_order'),('chunk-early-end',[start,end],'chunk_length'),
 ('chunk-bad-digest',[dict(start,sha256='0'*64),data,end],'chunk_checksum'),
 ('chunk-out-of-order',[start,dict(data,index='1')],'chunk_order'),
 ('chunk-length-over',[dict(start,total_bytes='1'),data],'limit_reached'),
 ('chunk-size-over',[dict(start,total_bytes='8388609')],'limit_reached'),
 ('chunk-empty',[dict(start,total_bytes='0')],'limit_reached'),
 ('chunk-stale-generation',[dict(start,execution_generation='5')],'stale_execution'),
 ('chunk-wrong-owner',[dict(start,owner=dict(scope,objective_id=uid(99)))],'stale_execution'),
 ('chunk-transfer-count',[start,dict(start,transfer_id=uid(10)),dict(start,transfer_id=uid(11))],'limit_reached')]:
    case(name,'chunks',dict(steps=[envelope(s) for s in steps]),expected,owner=scope,generation='4',output_hex=payload.hex() if name=='chunk-roundtrip' else '')
case('chunk-over-limit','frame',val(envelope(dict(data,hex='aa'*65537))),'invalid_schema')
case('chunk-at-limit','frame',val(envelope(dict(data,hex='aa'*65536))))
case('chunk-invalid-hex','frame',val(envelope(dict(data,hex='AA'))),'invalid_schema')
case('chunk-odd-hex','frame',val(envelope(dict(data,hex='a'))),'invalid_schema')

repeat=copy.deepcopy(command)
new_request=copy.deepcopy(command); new_request['body']['request_id']=uid(20)
changed=copy.deepcopy(new_request); changed['body']['command']['payload']['expected_draft_revision']='8'
changed_operation=copy.deepcopy(command); changed_operation['body']['operation_id']=uid(21)
for name,steps,expected in [('duplicate-request',[command,repeat],'ok'),('duplicate-operation',[command,new_request],'ok'),
 ('operation-conflict',[command,changed],'duplicate_conflict'),('request-conflict',[command,changed_operation],'duplicate_conflict')]:
    case(name,'history',dict(steps=steps),expected)
output=json.dumps(corpus,ensure_ascii=False,indent=2)+'\n'
path=root/'contracts/v1/fixtures.json'
if '--check' in sys.argv:
    if path.read_text()!=output: raise SystemExit('fixtures differ; run scripts/generate-fixtures.py')
else: path.write_text(output)
