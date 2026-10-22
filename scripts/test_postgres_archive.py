#!/usr/bin/env python3
"""Import a representative PostgreSQL archive without a running server.

Metadata comes from immutable original PostgreSQL migrations, independently of
the SQLite baseline; the restore therefore detects omitted or mistyped columns.
"""
import pathlib
import re
import sqlite3
import json
import uuid
import tempfile
import struct
import hashlib
import storage
root = pathlib.Path(storage.ROOT) / 'crates/zend/migrations'
source_sql = '\n'.join((p.read_text() for p in sorted(root.glob('*.sql'))))
source_sql = re.sub('--[^\\n]*', '', source_sql)
source_sql = re.sub('CREATE(?: OR REPLACE)? FUNCTION .*?\\$\\$.*?\\$\\$;', '', source_sql, flags=re.S | re.I)
tables = {m[1]: m[2].strip() for m in re.finditer('CREATE TABLE (\\w+)\\s*\\((.*?)\\);', source_sql, re.S | re.I)}
for match in re.finditer('ALTER TABLE (\\w+)\\s+(.*?);', source_sql, re.S | re.I):
    for column in re.split(',\\s*(?=ADD COLUMN)', match[2]):
        if column.lstrip().upper().startswith('ADD COLUMN '):
            tables[match[1]] += ',\n' + re.sub('^\\s*ADD COLUMN ', '', column, flags=re.I).strip()
pg = {}
for table, body in tables.items():
    parts = []
    start = depth = 0
    quote = False
    i = 0
    while i < len(body):
        ch = body[i]
        if ch == "'":
            if quote and i + 1 < len(body) and (body[i + 1] == "'"):
                i += 2
                continue
            quote = not quote
        if not quote:
            if ch == '(':
                depth += 1
            elif ch == ')':
                depth -= 1
            elif ch == ',' and depth == 0:
                parts.append(body[start:i].strip())
                start = i + 1
        i += 1
    parts.append(body[start:].strip())
    cols = []
    mapping = {'UUID': 'uuid', 'BIGSERIAL': 'int8', 'SERIAL': 'int4', 'BIGINT': 'int8', 'INT': 'int4', 'INTEGER': 'int4', 'SMALLINT': 'int2', 'BOOLEAN': 'bool', 'TIMESTAMPTZ': 'timestamptz', 'JSONB': 'jsonb', 'TEXT': 'text', 'TEXT[]': '_text', 'VECTOR(1536)': 'vector', 'TSVECTOR': 'tsvector', 'DOUBLE': 'float8'}
    for part in parts:
        tokens = part.split()
        if not tokens or tokens[0].upper().split('(')[0] in ('CHECK', 'PRIMARY', 'UNIQUE', 'HASH_PAYLOAD', 'HASH_VERSION'):
            continue
        if len(tokens) < 2:
            raise ValueError((table, part))
        typ = tokens[1].upper()
        if typ not in mapping:
            raise ValueError((table, part))
        cols.append({'name': tokens[0], 'type': mapping[typ]})
    pg[table] = cols
with tempfile.TemporaryDirectory() as temp:
    root = pathlib.Path(temp)
    source = root / 'source.db'
    with sqlite3.connect(source) as db:
        db.executescript(storage.SCHEMA.read_text())
        sid = uuid.uuid4().bytes
        turn = uuid.uuid4().bytes
        db.execute("INSERT INTO sessions(id,model,title,risk,created_at) VALUES(?,'faux/smoke','資料 ação',3,'2026-10-22T01:02:03.123456+00:00')", (sid,))
        payload = '{"role": "user", "content": "old"}'
        h = hashlib.sha256(('message' + payload).encode()).hexdigest()
        db.execute('INSERT INTO tape_events(session_id,kind,payload,seq,hash,hash_payload,hash_version) VALUES(?,?,?,?,?,?,1)', (sid, 'message', payload, 1, h, payload))
        db.execute("INSERT INTO turns(id,session_id,harness,worker,model,usage) VALUES(?,?,'x','faux','faux/smoke','null')", (turn, sid))
        db.execute("INSERT INTO memories(text,session_id,scores) VALUES('old',?,'true')", (sid,))
        job = db.execute('INSERT INTO jobs(name,kind,schedule,tz,created_by,context,skills) VALUES(\'a\',\'agent\',\'every 1h\',\'UTC\',\'owner\',\'["soul"]\',\'[]\') RETURNING id').fetchone()[0]
        db.execute("INSERT INTO job_runs(job_id,trigger,status,session_id,output) VALUES(?,'owner','ok',?,'result')", (job, sid))
        decision = db.execute('INSERT INTO decisions(point,input,answer) VALUES(\'x\',\'false\',\'"scalar"\') RETURNING id').fetchone()[0]
        db.execute('INSERT INTO s1_states(decision_id,state) VALUES(?,\'{"safe":true}\')', (decision,))
        db.execute("INSERT INTO s1_outcomes(decision_id,correct,source) VALUES(?,true,'owner')", (decision,))
        snapshot = uuid.uuid4().bytes
        db.execute('INSERT INTO source_ratings(scope,risk,answers,model,snapshot) VALUES(\'example.org\',2,\'{"trusted":true}\',\'faux\',?)', (snapshot,))
        db.execute("INSERT INTO action_gates(session_id,action,reason,decision) VALUES(?,'x','fixture','approved')", (sid,))
        db.execute("INSERT INTO model_calls(session_id,provider,model,input_tokens,output_tokens,cache_read,cache_write,cost_usd) VALUES(?,'faux','smoke',9007199254740993,2,0,0,0.125)", (sid,))
        db.execute("INSERT INTO search_docs(kind,ref,body,embedding,embed_model) VALUES('memory','m1','ação',?,'model')", (struct.pack('<1536f', *[0.5] + [0] * 1535),))
        rows = []
        for table, defs in pg.items():
            db.row_factory = sqlite3.Row
            for r in db.execute('SELECT * FROM ' + storage.ident(table)):
                row = {}
                for c in defs:
                    typ = c['type']
                    name = c['name']
                    value = r[name] if name in r.keys() else ''
                    if value is not None:
                        if typ == 'uuid':
                            value = str(uuid.UUID(bytes=value))
                        elif typ == 'bool':
                            value = bool(value)
                        elif typ == '_text':
                            value = json.loads(value)
                        elif typ == 'vector':
                            value = json.dumps(struct.unpack('<1536f', value))
                    row[name] = value
                extra = {'hash_payload': r['hash_payload']} if table == 'tape_events' else {}
                rows.append((table, row, extra))
        storage.write_archive(root / 'pg.jsonl', 'postgres', pg, rows)
    storage.restore(root / 'pg.jsonl', root / 'restored.db')
    with sqlite3.connect(root / 'restored.db') as db:
        assert db.execute('SELECT created_at,risk FROM sessions').fetchone() == ('2026-10-22T01:02:03.123456+00:00', 3)
        assert db.execute('SELECT input_tokens,cost_usd FROM model_calls').fetchone() == (9007199254740993, 0.125)
        assert db.execute('SELECT usage FROM turns').fetchone() == ('null',)
        assert db.execute('SELECT input,answer FROM decisions').fetchone() == ('false', '"scalar"')
        assert db.execute('SELECT length(embedding) FROM search_docs').fetchone() == (6144,)
        assert db.execute('SELECT snapshot,risk FROM source_ratings').fetchone() == (snapshot, 2)
        assert db.execute('SELECT correct FROM s1_outcomes').fetchone() == (1,)
        assert db.execute('SELECT decision FROM action_gates').fetchone() == ('approved',)
        assert db.execute('SELECT hash_version,hash_payload FROM tape_events').fetchone() == (1, payload)
        print('PASS: all', len(pg), 'PG table metadata,', len(rows), 'representative typed rows, canonical tape, JSON scalars/null, copied vector, UUIDs, arrays roundtrip')
