#!/usr/bin/env python3
"""Owner migration: exact archives, typed links, preview, held conflicts and retry after edits."""
import hashlib
import json
import pathlib
import sys
import urllib.request

url, token, home = sys.argv[1:]
root = pathlib.Path(home) / '.zenbot/global/wiki'
root.mkdir(parents=True, exist_ok=True)


def call(action, **args):
    req = urllib.request.Request(url + '/api/brain',
        data=json.dumps(dict(action=action, **args)).encode(),
        headers={'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json'})
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.load(r)


def ident(slug):
    return 'legacy-wiki-' + hashlib.sha256(slug.encode()).hexdigest()[:24]


def check(ok, message):
    assert ok, message
    print('    ok: ' + message)


original = ('---\ntype: person\naliases: [Alex]\n---\n# Alex\n\n'
            'Freeform notes about [[launch|Launch]] and [[missing]].\n\n---\n## Timeline\n'
            '- **2026-10-01** | session x (web) — Tentative meeting claim.\n\n')
(root / 'alex.md').write_text(original)
(root / 'launch.md').write_text('# Launch\n\nDecision and reasoning.\n')
(root / 'large.md').write_text('x' * (65536 + 1))
(root / 'index.md').write_text('# Original index\n')
preview = call('migrate_legacy')
check(preview['held'] == 1 and len(preview['pages']) == 3, 'preview reports oversized pages')
check(not (root / 'sources').exists() and (root / 'alex.md').read_text() == original,
      'preview leaves original files untouched')
result = call('migrate_legacy', apply=True)
check(result['migrated'] == 2 and result['held'] == 1, 'valid pages migrate while oversized pages stay held')
a = call('get', id=ident('alex'))
check(a['store'] == 'knowledge' and a['kind'] == 'source' and a['status'] == 'provisional',
      'old personal notes remain attributed sources rather than verified CRM facts')
check(a['body'] == original.strip() and a['metadata']['legacy']['aliases'] == ['Alex'],
      'all original prose and aliases survive')
archive = pathlib.Path(a['metadata']['legacy']['archive'])
check(archive.read_bytes() == original.encode(), 'archive retains exact original bytes')
check(archive.stat().st_mode & 0o777 == 0o600, 'archive is private')
check(a['metadata']['links'] == [{'relation': 'related_to', 'target': ident('launch')}],
      'resolvable old links become typed links; unresolved links remain in original prose')
check(not (root / 'alex.md').exists() and (root / 'large.md').exists(),
      'only successfully committed originals leave the flat wiki')
record_path = root / 'sources' / (ident('alex') + '.md')
record = record_path.read_text().replace('Tentative meeting claim.', 'Owner corrected the meeting claim.')
record_path.write_text(record)
(root / 'alex.md').write_text(original)
retry = call('migrate_legacy', apply=True)
check(retry['migrated'] == 1 and record_path.read_text() == record,
      'retry preserves owner edits to the migrated record')
(root / 'alex.md').write_text(original + 'A later legacy edit.\n')
conflict = call('migrate_legacy', apply=True)
check(conflict['held'] == 2 and record_path.read_text() == record and (root / 'alex.md').exists(),
      'changed originals conflict without overwriting either version')
(root / 'copy.md').write_text(original)
collision = call('migrate_legacy', apply=True)
check(collision['held'] == 3 and (root / 'copy.md').exists(),
      'duplicate knowledge names stay held for owner resolution')
check((root / 'index.md').read_text() == '# Original index\n', 'historical index is preserved')
# Invalid typed files are reported only by count in the compatibility upkeep job.
(root / 'sources' / 'invalid.md').write_text('not a record')
