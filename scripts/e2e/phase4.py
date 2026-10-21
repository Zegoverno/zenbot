#!/usr/bin/env python3
"""Second-brain owner workflows against a disposable kernel; no model/provider credentials."""
import json
import pathlib
import sys
import urllib.request
import urllib.error

URL, TOKEN, HOME, SESSION = sys.argv[1:]
checks = 0

def call(action, **args):
    req = urllib.request.Request(URL + '/api/brain', data=json.dumps(dict(action=action, **args)).encode(),
        headers={'Authorization': 'Bearer '+TOKEN, 'Content-Type': 'application/json'})
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.load(r)

def check(condition, message):
    global checks
    assert condition, message
    checks += 1
    print('    ok: ' + message)

def refuses(action, **args):
    try:
        call(action, **args)
    except urllib.error.HTTPError as e:
        assert e.code == 400, (e.code, e.read())
        return True
    return False

def put(id, kind, title, body, store='knowledge', **meta):
    return call('put', record=dict(id=id, store=store, kind=kind, title=title, body=body, metadata=meta))

put('meeting-source', 'source', 'Pilot discussion transcript',
    'Synthetic test transcript. Ana: I will confirm capacity. Jose: I will send a pilot proposal. No deal was signed.',
    evidence=[dict(source='fixture transcript', locator='lines 1-3', basis='observed')])
put('ana-partner', 'person', 'Ana Example — partner', 'Partnership contact; identity resolved explicitly.', store='entity', aliases=['Ana at Example'])
put('pilot-project', 'project', 'Pilot partnership', 'Potential pilot. Not an agreed commercial contract.', store='entity')
put('pilot-meeting', 'event', 'Pilot discussion', 'Discussed capacity and a proposal; agreement remains open.',
    evidence=[dict(source='meeting-source', locator='lines 1-3', basis='observed')],
    links=[dict(relation='about', target='ana-partner'), dict(relation='about', target='pilot-project')])
check(call('get', id='ana-partner')['revision'] == 1, 'entity has a stable revision')
check(refuses('put', record=dict(id='different-ana', store='entity', kind='person', title='Other person', body='Ambiguous', metadata={'aliases':['Ana at Example']})), 'ambiguous aliases are rejected')
check(refuses('put', record=dict(id='../escape', store='knowledge', kind='concept', title='Bad', body='Bad')), 'record IDs cannot escape the storage root')
check(refuses('put', record=dict(id='unsupported', store='knowledge', kind='claim', title='Unsupported', body='Unsupported', metadata={'links':[{'relation':'supports','target':'pilot-project'}]})), 'support links require evidence')
check(refuses('put', record=dict(id='missing-link', store='knowledge', kind='concept', title='Missing link', body='Text', metadata={'links':[{'relation':'related_to','target':'nonexistent'}]})), 'unknown targets are rejected')
check(any(e['target']=='ana-partner' for e in call('neighbors',id='pilot-meeting')['links']), 'graph exposes meeting/person relationship')
check(len(call('list',store='entity')['records'])==2, 'entities can be enumerated independently')
record=call('get',id='pilot-project')
record['body']='Pilot remains paused until capacity is confirmed.'
check(refuses('put',record=record), 'updates require an expected revision')
call('put',record=record,expected_revision=1)
check(call('get',id='pilot-project')['revision']==2, 'revision-safe update publishes')
check(refuses('put',record=record,expected_revision=1), 'stale updates cannot overwrite current content')
check(call('history',id='pilot-project')['revisions'][0]['revision']==1, 'previous record content remains recoverable')
# Manual file changes must not be silently served from a stale database index.
path=pathlib.Path(HOME)/'.zenbot/global/entities/projects/pilot-project.md'
original=path.read_text()
path.write_text(original.replace('Pilot remains paused until capacity is confirmed.', 'Owner manually corrected the pilot account.'))
check(refuses('get',id='pilot-project'), 'changed file is withheld pending import')
check(any(i['id']=='pilot-project' for i in call('lint')['issues']), 'lint identifies the manual edit')
call('import',id='pilot-project')
check('manually corrected' in call('get',id='pilot-project')['body'], 'explicit import reconciles file and index')
check(not call('lint')['issues'], 'published files and schema are consistent')
# Source/claim simulation, preserving distinct evidence and interpretations.
put('two-processes','concept','Two-process account','Framework distinguishes relatively automatic and deliberate processing.',
    subjects=['cognitive-psychology'],evidence=[dict(source='example reading',locator='inspected introduction',basis='interpretation')])
put('system-one','concept','System 1 in the example account','Automatic processing in this framework.',
    links=[dict(relation='part_of',target='two-processes')])
check(any(e['target']=='two-processes' for e in call('neighbors',id='system-one')['links']), 'concept graph preserves a typed parent link')
check(call('search',query='deliberate')['records'][0]['id']=='two-processes', 'owner searches inspect full matching evidence')
# Versioned evidence and anchored claims remain attributable through corrections.
put('anchored-account','concept','Anchored account','## Claim {#limited}\nA claim with qualifications.')
put('counterclaim','claim','Counterclaim','An alternative interpretation.',
    links=[dict(relation='contradicts',target='anchored-account#limited',evidence=[dict(source='meeting-source',locator='line 3',basis='interpretation')])])
check(call('neighbors',id='counterclaim')['links'][0]['target_anchor']=='limited', 'claim anchors survive graph projection')
check(call('get',id='counterclaim')['metadata']['links'][0]['evidence'][0]['revision']==1, 'edge evidence freezes its source revision')
check(refuses('put',record=dict(id='bad-anchor',store='knowledge',kind='concept',title='Bad anchor',body='Text',metadata={'links':[{'relation':'related_to','target':'anchored-account#missing'}]})), 'missing claim anchors are refused')
# Publication receipts recover deleted projections without overwriting a manual correction.
path.unlink()
check(refuses('get',id='pilot-project'), 'missing file is withheld')
call('repair',id='pilot-project')
check('manually corrected' in call('get',id='pilot-project')['body'], 'publication receipt repairs a missing file')
from concurrent.futures import ThreadPoolExecutor
record=call('get',id='pilot-project')
def race_update(label):
    candidate=dict(record,body='Concurrent correction '+label)
    try:
        call('put',record=candidate,expected_revision=record['revision'])
        return True
    except urllib.error.HTTPError as e:
        assert e.code==400
        return False
with ThreadPoolExecutor(max_workers=2) as executor:
    winners=list(executor.map(race_update,['one','two']))
check(sum(winners)==1, 'concurrent corrections have exactly one revision winner')
check(call('schema')['relations'], 'owner can inspect the schema contract')
check('proposals' in call('dream'), 'dream returns proposals without publishing facts')
check(call('work'), 'processing history is inspectable')
print(f'    {checks} phase4 owner workflow checks passed')
