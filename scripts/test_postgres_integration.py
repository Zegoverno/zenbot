#!/usr/bin/env python3
"""Real PostgreSQL/pgvector -> JSONL -> SQLite integration, on a fresh disposable database only.

Set PGHOST/PGPORT/PGUSER/PGPASSWORD and PGDATABASE=zen_migration_test (or that prefix plus
an underscore suffix). Requires psql and pgvector; never uses the owner's DATABASE_URL.
All durable tables are compared, including default rows inserted by historical migrations.
"""
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import struct
import subprocess
import tempfile
import uuid
import storage

SID='11111111-1111-4111-8111-111111111111'
READER='22222222-2222-4222-8222-222222222222'
TURN='33333333-3333-4333-8333-333333333333'
SNAPSHOT='44444444-4444-4444-8444-444444444444'
VECTOR=[0.5,-0.25,0.125]+[0.0]*1533


def pg(sql):
    result=subprocess.run(['psql','-X','-qAt','-v','ON_ERROR_STOP=1'],input=sql,text=True,capture_output=True)
    if result.returncode:
        raise RuntimeError('Disposable PostgreSQL fixture failed: '+result.stderr)
    return result.stdout.strip()


def archive_rows(path):
    result={}
    with open(path) as src:
        for raw in src:
            obj=json.loads(raw)
            if 'table' in obj:
                result.setdefault(obj['table'],[]).append(json.dumps(obj,sort_keys=True))
    return {table:sorted(rows) for table,rows in result.items()}


def main():
    name=os.environ.get('PGDATABASE','')
    if name!='zen_migration_test' and not name.startswith('zen_migration_test_'):
        raise SystemExit('Refusing: PGDATABASE must identify a disposable zen_migration_test database')
    if pg("SELECT count(*) FROM information_schema.tables WHERE table_schema='public' AND table_type='BASE TABLE'")!='0':
        raise SystemExit('Refusing: integration database must be empty')
    migrations=sorted((storage.ROOT/'crates/zend/migrations').glob('*.sql'))
    sql=['BEGIN; CREATE TABLE _sqlx_migrations(version bigint PRIMARY KEY,description text NOT NULL,installed_on timestamptz NOT NULL DEFAULT now(),success boolean NOT NULL,checksum bytea NOT NULL,execution_time bigint NOT NULL);']
    for migration in migrations:
        sql.append(migration.read_text())
        version=int(migration.name.split('_')[0])
        checksum=hashlib.sha384(migration.read_bytes()).hexdigest()
        sql.append(f"INSERT INTO _sqlx_migrations(version,description,success,checksum,execution_time) VALUES({version},'fixture',true,decode('{checksum}','hex'),0);")
    sql.append('COMMIT;')
    pg('\n'.join(sql))
    vector=json.dumps(VECTOR,separators=(',',':'))
    pg(f"""
BEGIN;
INSERT INTO sessions(id,model,title,risk,created_at) VALUES
 ('{SID}','faux/smoke','資料 ação',3,'2026-10-22T01:02:03.123456+00:00'),
 ('{READER}','faux/smoke','reader',1,now());
INSERT INTO session_inputs(reader,writer) VALUES('{READER}','{SID}');
INSERT INTO tape_events(session_id,kind,payload) VALUES
 ('{SID}','message','{{"z":null,"role":"user","content":"ação 資料","nested":{{"a":true,"b":1.0}}}}'),
 ('{SID}','message','{{"role":"assistant","content":[{{"type":"text","text":"answer"}}]}}');
SELECT zen_rechain('{SID}');
INSERT INTO turns(id,session_id,harness,worker,model,usage) VALUES('{TURN}','{SID}','test','faux','faux/smoke','null');
INSERT INTO memories(text,session_id,scores) VALUES('retained provenance','{SID}','true');
INSERT INTO jobs(name,kind,schedule,tz,created_by,context,skills) VALUES('migration-fixture','agent','every 1h','UTC','owner',ARRAY['soul','USER.md'],ARRAY['work/verify']);
INSERT INTO job_runs(job_id,trigger,status,session_id,output) SELECT id,'owner','ok','{SID}','retained report' FROM jobs WHERE name='migration-fixture';
INSERT INTO decisions(point,input,answer) VALUES('migration-fixture','false','"scalar"');
INSERT INTO s1_states(decision_id,state) SELECT id,'{{"safe":true}}' FROM decisions WHERE point='migration-fixture';
INSERT INTO s1_outcomes(decision_id,correct,source) SELECT id,true,'owner' FROM decisions WHERE point='migration-fixture';
INSERT INTO source_ratings(scope,risk,answers,model,snapshot) VALUES('example.org',2,'{{"trusted":true}}','faux','{SNAPSHOT}');
INSERT INTO action_gates(session_id,action,reason,decision) VALUES('{SID}','fixture','migration','approved');
INSERT INTO model_calls(session_id,provider,model,input_tokens,output_tokens,cache_read,cache_write,cost_usd) VALUES('{SID}','faux','smoke',9007199254740993,2,0,0,0.125);
INSERT INTO search_docs(kind,ref,body,embedding,embed_model) VALUES('memory','fixture','ação 資料','{vector}'::vector,'openai/text-embedding-3-small');
INSERT INTO contamination(cause,affected) VALUES('{SID}','{SID}');
COMMIT;
""")
    with tempfile.TemporaryDirectory() as temp:
        root=Path(temp)
        archive=root/'postgres.jsonl'
        storage.postgres_export(archive,['psql'])
        storage.restore(archive,root/'restored.sqlite3')
        # Export again to establish that neither export nor restore changed PostgreSQL state.
        storage.postgres_export(root/'after.jsonl',['psql'])
        assert archive_rows(archive)==archive_rows(root/'after.jsonl'),'source PostgreSQL changed'
        header=json.loads(archive.read_text().splitlines()[0])
        footer=json.loads(archive.read_text().splitlines()[-1])
        count_queries=["SELECT '"+table.replace("'","''")+"' AS name,count(*) AS n FROM "+storage.ident(table) for table in header['tables']]
        pg_counts=json.loads(pg('SELECT jsonb_object_agg(name,n) FROM ('+' UNION ALL '.join(count_queries)+') counts'))
        assert footer['counts']==pg_counts,'table counts differ from PostgreSQL'
        with sqlite3.connect(root/'restored.sqlite3') as db:
            storage.verify_tape(db)
            for table,count in pg_counts.items():
                assert db.execute('SELECT count(*) FROM '+storage.ident(table)).fetchone()[0]==count,table
            # Compare every archived cell against the actual restored values, independently of
            # the importer's insertion loop. PostgreSQL-generated full-text columns are rebuilt.
            for raw in archive.read_text().splitlines()[1:-1]:
                obj=json.loads(raw)
                table=obj['table']
                types={c['name']:c['type'] for c in header['tables'][table]}
                expected={name:storage.convert(value,types[name]) for name,value in obj['row'].items() if name not in ('tsv','fts_en','fts_pt')}
                if table=='tape_events':
                    expected.update(hash_payload=obj['hash_payload'],hash_version=1)
                where=' AND '.join(storage.ident(k)+' IS ?' for k in expected)
                assert db.execute('SELECT count(*) FROM '+storage.ident(table)+' WHERE '+where,list(expected.values())).fetchone()[0]==1,table
            assert db.execute('SELECT embedding FROM search_docs WHERE ref=\'fixture\'').fetchone()[0]==struct.pack('<1536f',*VECTOR)
            assert db.execute('SELECT usage FROM turns WHERE id=?',(uuid.UUID(TURN).bytes,)).fetchone()[0]=='null'
            assert db.execute("SELECT input,answer FROM decisions WHERE point='migration-fixture'").fetchone()==('false','"scalar"')
            assert db.execute('SELECT input_tokens,cost_usd FROM model_calls').fetchone()==(9007199254740993,0.125)
            original=json.loads(pg("SELECT row_to_json(m) FROM (SELECT risk,rating_reason,rating_writer_risk FROM memories WHERE text='retained provenance') m"))
            assert db.execute("SELECT risk,rating_reason,rating_writer_risk FROM memories WHERE text='retained provenance'").fetchone()==tuple(original.values())
            for sid in (SID,READER):
                pg_denied=pg(f"SELECT zen_session_contaminated('{sid}')")=='t'
                sqlite_denied=bool(db.execute('SELECT EXISTS(SELECT 1 FROM zen_contaminated_sessions WHERE id=?)',(uuid.UUID(sid).bytes,)).fetchone()[0])
                assert pg_denied==sqlite_denied==True
            assert db.execute("SELECT count(*) FROM search_docs_fts WHERE search_docs_fts MATCH 'ação'").fetchone()[0]==1
            new_id=db.execute("INSERT INTO tape_events(session_id,kind,payload) VALUES(?,'fixture','{}') RETURNING id",(uuid.UUID(READER).bytes,)).fetchone()[0]
            assert new_id>int(pg('SELECT max(id) FROM tape_events'))
        print(f'PASS: real PostgreSQL {len(migrations)} migrations, {len(pg_counts)} durable tables, canonical hashes, copied vectors, every restored cell and source retention')


if __name__=='__main__':
    main()
