#!/usr/bin/env python3
"""Revision, archive and projection recovery invariants on a disposable second brain."""
import hashlib
import json
import pathlib
import sys
import urllib.error
import urllib.request

URL, TOKEN, HOME, SESSION, ID = sys.argv[1:]
root = pathlib.Path(HOME) / '.zenbot/global'
checks = 0

def call(action, **args):
    request = urllib.request.Request(URL+'/api/brain', data=json.dumps(dict(action=action, **args)).encode(),
        headers={'Authorization':'Bearer '+TOKEN,'Content-Type':'application/json'})
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.load(response)

def check(condition, message):
    global checks
    assert condition, message
    checks += 1
    print('    ok: '+message)

def edit(action, **args):
    revision = call('get', id=ID)['revision']
    return call(action, id=ID, expected_revision=revision, **args)

edit('annotate', details={'owner':'Ana','state':'open','due':'2026-11-01'},event_at='2026-10-20T10:00:00Z')
record = call('get', id=ID)
check(record['details']['state']=='open' and record['event_at'].startswith('2026-10-20'), 'commitment metadata preserves event time independently of recording time')
edit('pin')
long_path = root/'memory/records'/f'{ID}.md'
check(long_path.exists() and 'confirmed capacity' in long_path.read_text(), 'pin publishes a long-term projection')
revision = call('get', id=ID)['revision']
edit('archive')
check('Projection withdrawn' in long_path.read_text(), 'archive withdraws the old active projection')
check('confirmed capacity' in (root/'memory/archive'/f'{ID}.md').read_text(), 'archive retains original evidence')
check(any(r['id']==int(ID[1:]) for r in call('list',store='memory',tier='archived')['records']), 'explicit archive enumeration retrieves retained memory')
edit('undo', revision=revision)
check(call('get', id=ID)['tier']=='long', 'undo restores the reviewed tier')
check('Projection withdrawn' in (root/'memory/archive'/f'{ID}.md').read_text(), 'undo withdraws the superseded archive projection')
# A manual edit must survive until its exact bytes have been reviewed.
view = root/'MEMORY.md'
manual = 'Owner manual scratch: preserve this text.\n'
view.write_text(manual)
result = edit('annotate', details={'state':'reviewed'})
check(result['ok'] and result['projection_error'], 'durable edits acknowledge a blocked projection separately')
check(view.read_text()==manual, 'manual generated-view edits are never silently overwritten')
try:
    call('repair_projection',path='MEMORY.md',expected_digest='stale')
    raise AssertionError('stale repair accepted')
except urllib.error.HTTPError as error:
    check(error.code==400, 'projection repair requires an exact reviewed digest')
call('repair_projection',path='MEMORY.md',expected_digest=hashlib.sha256(manual.encode()).hexdigest())
check(any(p.read_text()==manual for p in (root/'memory/conflicts').glob('*.md')), 'reviewed manual bytes remain recoverable')
check(view.read_text()!=manual, 'reviewed projection repair restores the canonical view')
# Unicode content and labels must fit the same conservative context budget.
call('put',record={'id':'unicode-evidence','store':'knowledge','kind':'concept','title':'Ana capacidade ação',
    'body':('Ana confirmou capacidade e ação.\n\n'*1000),'metadata':{}})
pack = call('retrieve',session=SESSION,query='Ana capacidade ação')
check(len((pack['context'] or '').encode())<=4000, 'context including labels respects the UTF-8 byte budget')
receipt = pack['receipt']['contexts'][0]['receipt']
check(receipt['coverage']=='unverified', 'retrieval never equates top-K evidence with complete coverage')
check(call('get',id=ID)['details']['state']=='reviewed', 'projection failure did not roll back the durable memory edit')
print(f'    {checks} phase4 memory recovery checks passed')
