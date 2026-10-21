#!/usr/bin/env python3
"""Record files through the owner's `brain` actions, on a disposable kernel: identities, typed links,
revisions, the owner's own edits of the files, and the index rebuilt from them."""
import json
import pathlib
import sys
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor

URL, TOKEN, HOME = sys.argv[1:]
root = pathlib.Path(HOME) / '.zenbot/global'
checks = 0


def call(action, **args):
    req = urllib.request.Request(URL + '/api/brain', data=json.dumps(dict(action=action, **args)).encode(),
                                 headers={'Authorization': 'Bearer ' + TOKEN, 'Content-Type': 'application/json'})
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
    'Synthetic transcript. Ana: I will confirm capacity. Jose: I will send a pilot proposal. No deal was signed.',
    evidence=[dict(source='fixture transcript', locator='lines 1-3', basis='observed')])
put('ana-partner', 'person', 'Ana Example', 'Partnership contact.', store='entity', aliases=['Ana at Example'])
put('pilot-project', 'project', 'Pilot partnership', 'A potential pilot, not a signed contract.', store='entity', tags=['pilot'])
put('pilot-meeting', 'event', 'Pilot discussion with Ana', 'Discussed capacity and a proposal; agreement remains open.',
    evidence=[dict(source='meeting-source', locator='lines 1-3', basis='observed')],
    links=[dict(relation='about', target='ana-partner'), dict(relation='about', target='pilot-project')])
check((root / 'entities/people/ana-partner.md').read_text().startswith('---\n{'), 'a record is a Markdown file with its metadata on top')
check(call('get', id='ana-partner')['revision'] == 1, 'a new record is at revision 1')
check(refuses('put', record=dict(id='other-ana', store='entity', kind='person', title='Someone', body='Ambiguous', metadata={'aliases': ['Ana at Example']})), 'a name that belongs to another record is refused')
check(refuses('put', record=dict(id='../escape', store='knowledge', kind='concept', title='Bad', body='Bad')), 'an id cannot leave its folder')
check(refuses('put', record=dict(id='unsupported', store='knowledge', kind='claim', title='Unsupported', body='Text', metadata={'links': [{'relation': 'supports', 'target': 'pilot-project'}]})), 'a supports link needs evidence')
check(refuses('put', record=dict(id='dangling', store='knowledge', kind='concept', title='Dangling', body='Text', metadata={'links': [{'relation': 'related_to', 'target': 'nowhere'}]})), 'a link to a missing record is refused')
check(refuses('put', record=dict(id='t', store='memory', kind='concept', title='T', body='T')), 'a kind belongs to its store')
links = call('get', id='pilot-meeting')['links']
check({'from': 'pilot-meeting', 'relation': 'about', 'to': 'ana-partner'} in links, 'get shows a record\'s typed links')
check({'from': 'pilot-meeting', 'relation': 'about', 'to': 'ana-partner'} in call('get', id='ana-partner')['links'], 'and the links pointing at it')
check(len(call('list', store='entity')['records']) == 2, 'records list by store')
check([r['id'] for r in call('list', tag='pilot')['records']] == ['pilot-project'], 'and by tag')
record = call('get', id='pilot-project')
record['body'] = 'The pilot is paused until capacity is confirmed.'
check(refuses('put', record=record), 'an update names the revision it read')
call('put', record=record, expected_revision=1)
check(call('get', id='pilot-project')['revision'] == 2, 'an update makes a new revision')
check(refuses('put', record=record, expected_revision=1), 'a stale update is refused')
check('A potential pilot' in (pathlib.Path(HOME) / '.zenbot/backups/brain/pilot-project-r1.md').read_text(), 'the previous version is backed up')
# The files are the source of truth: the owner's edit is what the next read sees.
path = root / 'entities/projects/pilot-project.md'
path.write_text(path.read_text().replace('The pilot is paused until capacity is confirmed.', 'The owner corrected this by hand.'))
check('corrected this by hand' in call('get', id='pilot-project')['body'], 'the owner edits a file by hand and the next read sees it')
call('index')
check(not call('lint')['issues'], 'the index agrees with the files')
(root / 'entities/projects/broken.md').write_text('---\n{not json\n---\nbody\n')
check(any('broken.md' in str(i.get('path', '')) for i in call('lint')['issues']), 'lint names a file that does not parse')
(root / 'entities/projects/broken.md').unlink()
(root / 'entities/projects/pilot-project.md').rename(root / 'pilot-project.md')
call('index')
check(refuses('get', id='pilot-project'), 'a record whose file is gone leaves the index')
(root / 'pilot-project.md').rename(root / 'entities/projects/pilot-project.md')
call('index')
record = call('get', id='pilot-project')


def race(label):
    try:
        call('put', record=dict(record, body='Concurrent correction ' + label), expected_revision=record['revision'])
        return True
    except urllib.error.HTTPError as e:
        assert e.code == 400
        return False


with ThreadPoolExecutor(max_workers=2) as pool:
    winners = list(pool.map(race, ['one', 'two']))
check(sum(winners) == 1, 'of two updates from the same revision, exactly one wins')
check('relations' in call('schema'), 'the schema is inspectable')
print(f'    {checks} record checks passed')
