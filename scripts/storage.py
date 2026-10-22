#!/usr/bin/env python3
"""Private, versioned durable-state archives. PostgreSQL is read only; imports publish atomically.

PostgreSQL export uses one repeatable-read transaction through psql (or an explicit psql
command after --). SQLite export/backup use the online backup API. Archives contain secrets.
The caller must quiesce the kernel before final cutover; this tool never changes live config.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import shlex
import sqlite3
import struct
import subprocess
import tempfile
import uuid
from urllib.parse import urlsplit, unquote, parse_qsl

FORMAT = 'zenbot-state-v1'
ROOT = Path(__file__).resolve().parents[1]
SCHEMA = ROOT / 'crates/zend/sqlite-migrations/0001_init.sql'


def ident(value):
    return '"' + value.replace('"', '""') + '"'


def line(value):
    return json.dumps(value, ensure_ascii=False, separators=(',', ':')) + '\n'


def private_new(path):
    return os.fdopen(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), 'w')


def tables(db):
    return [r[0] for r in db.execute("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE '%_fts%' AND name != '_sqlx_migrations' ORDER BY name")]


def encode(value):
    return {'$bytes': value.hex()} if isinstance(value, bytes) else value


def write_archive(path, backend, metadata, rows):
    digest = hashlib.sha256()
    counts = dict.fromkeys(metadata, 0)
    created = False
    try:
        out = private_new(path)
        created = True
        with out:
            header = line({'format': FORMAT, 'backend': backend, 'tables': metadata})
            out.write(header)
            digest.update(header.encode())
            for table, row, extra in rows:
                record = line({'table': table, 'row': row, **extra})
                digest.update(record.encode())
                out.write(record)
                counts[table] += 1
            out.write(line({'end': True, 'counts': counts, 'sha256': digest.hexdigest()}))
            out.flush()
            os.fsync(out.fileno())
    except BaseException:
        if created:
            Path(path).unlink(missing_ok=True)
        raise


def sqlite_export(source, archive):
    with sqlite3.connect(f'file:{Path(source).resolve()}?mode=ro', uri=True) as live, sqlite3.connect(':memory:') as db:
        live.backup(db)
        meta = {t: [{'name': c[1], 'type': c[2]} for c in db.execute('PRAGMA table_info(' + ident(t) + ')')] for t in tables(db)}
        def rows():
            for t, cols in meta.items():
                for row in db.execute('SELECT * FROM ' + ident(t)):
                    yield t, {c['name']: encode(v) for c, v in zip(cols, row)}, {}
        write_archive(archive, 'sqlite', meta, rows())


def postgres_export(archive, command):
    # Server-side dynamic SQL keeps table discovery and every row in the SAME snapshot.
    sql = r"""
SET TIME ZONE 'UTC';
CREATE TEMP TABLE zen_export_lines(n bigserial, data jsonb);
BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY;
INSERT INTO zen_export_lines(data) SELECT jsonb_build_object('format','zenbot-state-v1','backend','postgres','tables',jsonb_object_agg(table_name,cols)) FROM (
 SELECT table_name,jsonb_agg(jsonb_build_object('name',column_name,'type',udt_name) ORDER BY ordinal_position) cols
 FROM information_schema.columns WHERE table_schema='public' AND table_name != '_sqlx_migrations' AND table_name IN (SELECT table_name FROM information_schema.tables WHERE table_schema='public' AND table_type='BASE TABLE') GROUP BY table_name) s;
DO $export$ DECLARE t text; extra text; canonical text; BEGIN
 FOR t IN SELECT table_name FROM information_schema.tables WHERE table_schema='public' AND table_type='BASE TABLE' AND table_name != '_sqlx_migrations' ORDER BY table_name LOOP
 SELECT string_agg(format('%L, r.%I::text',column_name,column_name), ',') INTO canonical FROM information_schema.columns WHERE table_schema='public' AND table_name=t AND udt_name IN ('json','jsonb');
 canonical := CASE WHEN canonical IS NULL THEN 'to_jsonb(r)' ELSE 'to_jsonb(r) || jsonb_build_object(' || canonical || ')' END;
 extra := CASE WHEN t='tape_events' THEN ', ''hash_payload'', r.payload::text' ELSE '' END;
 EXECUTE format('INSERT INTO zen_export_lines(data) SELECT jsonb_build_object(''table'', %L, ''row'', %s%s) FROM %I r',t,canonical,extra,t);
 END LOOP;
END $export$;
SELECT data::text FROM zen_export_lines ORDER BY n;
COMMIT;
"""
    # A spool file avoids holding a potentially large database in Python memory.
    with tempfile.TemporaryFile(mode='w+') as spool:
        subprocess.run(command + ['-X', '-qAt', '-v', 'ON_ERROR_STOP=1'], input=sql, text=True, stdout=spool, check=True)
        spool.seek(0)
        header = json.loads(next(spool))
        def rows():
            for raw in spool:
                obj = json.loads(raw)
                yield obj['table'], obj['row'], {k:v for k,v in obj.items() if k not in ('table','row')}
        write_archive(archive, 'postgres', header['tables'], rows())


def convert(value, pg_type):
    if value is None:
        return None
    if isinstance(value, dict) and set(value) == {'$bytes'}:
        return bytes.fromhex(value['$bytes'])
    if pg_type in ('json','jsonb'):
        if not isinstance(value, str):
            raise ValueError('canonical PostgreSQL JSON text required')
        json.loads(value)
        return value
    if pg_type == 'uuid':
        return uuid.UUID(value).bytes
    if pg_type == 'bytea':
        if not value.startswith('\\x'):
            raise ValueError('unsupported bytea encoding')
        return bytes.fromhex(value[2:])
    if pg_type == 'vector':
        values = json.loads(value) if isinstance(value, str) else value
        if len(values) != 1536:
            raise ValueError('embedding must contain 1536 dimensions')
        if any(isinstance(v,bool) or not isinstance(v,(int,float)) or not math.isfinite(v) for v in values):
            raise ValueError('embedding contains nonfinite or nonnumeric values')
        packed = struct.pack('<1536f', *values)
        if not all(math.isfinite(v) for v in struct.unpack('<1536f',packed)):
            raise ValueError('embedding overflows float32')
        return packed
    if isinstance(value, (dict, list)):
        return json.dumps(value, ensure_ascii=False, separators=(',', ':'))
    return value


def verify_tape(db):
    if 'tape_events' not in tables(db):
        return
    previous = {}
    for sid, event_id, seq, parent, kind, payload, stored, actual_payload, version in db.execute('SELECT session_id,id,seq,parent,kind,hash_payload,hash,payload,hash_version FROM tape_events ORDER BY session_id,seq'):
        prev_id, prev_hash, prev_seq = previous.get(sid, (None, '', 0))
        if parent != prev_id or seq != prev_seq + 1 or payload is None:
            raise ValueError('invalid tape chain linkage or missing canonical payload')
        if version not in (1, 2) or json.dumps(json.loads(actual_payload),sort_keys=True,separators=(',',':')) != json.dumps(json.loads(payload),sort_keys=True,separators=(',',':')):
            raise ValueError('tape payload differs from hashed payload or unknown hash format')
        digest = hashlib.sha256((prev_hash + kind + payload).encode()).hexdigest()
        if digest != stored:
            raise ValueError('tape hash mismatch')
        previous[sid] = (event_id, stored, seq)


def restore(archive, destination, schema=SCHEMA):
    dest = Path(destination)
    if dest.exists():
        raise ValueError('destination already exists; refusing to overwrite')
    dest.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    fd, tmp = tempfile.mkstemp(prefix=dest.name + '.', dir=dest.parent)
    os.close(fd)
    try:
        with sqlite3.connect(tmp) as db, open(archive) as src:
            db.executescript(Path(schema).read_text())
            db.execute('PRAGMA foreign_keys=OFF')
            header_raw = next(src)
            header = json.loads(header_raw)
            if header.get('format') != FORMAT or header.get('backend') not in ('sqlite', 'postgres'):
                raise ValueError('unknown archive format')
            meta = header['tables']
            actual = set(tables(db))
            if set(meta) != actual:
                raise ValueError(f'schema tables differ: {set(meta) ^ actual}')
            for table, definitions in meta.items():
                names = {c['name'] for c in definitions}
                if len(names) != len(definitions):
                    raise ValueError('duplicate column metadata: ' + table)
                target = {c[1] for c in db.execute('PRAGMA table_info(' + ident(table) + ')')}
                if header['backend'] == 'postgres':
                    if table == 'search_docs':
                        names -= {'tsv', 'fts_en', 'fts_pt'}
                    if table == 'tape_events':
                        target -= {'hash_payload', 'hash_version'}
                if names != target:
                    raise ValueError('schema columns differ: ' + table)
                target_types = {c[1]: c[2] for c in db.execute('PRAGMA table_info(' + ident(table) + ')')}
                pg_types = {'uuid':'BLOB','vector':'BLOB','bytea':'BLOB','int2':'INTEGER','int4':'INTEGER','int8':'INTEGER','bool':'INTEGER','float4':'REAL','float8':'REAL','numeric':'REAL','text':'TEXT','varchar':'TEXT','bpchar':'TEXT','json':'TEXT','jsonb':'TEXT','timestamptz':'TEXT','timestamp':'TEXT','date':'TEXT','_text':'TEXT'}
                for column in definitions:
                    if column['name'] not in names:
                        continue
                    declared = column['type']
                    expected = pg_types.get(declared) if header['backend']=='postgres' else declared
                    if expected != target_types[column['name']]:
                        raise ValueError('schema column type differs: ' + table + '.' + column['name'])
            counts = dict.fromkeys(meta, 0)
            digest = hashlib.sha256(header_raw.encode())
            ended = False
            db.execute('BEGIN IMMEDIATE')
            triggers = db.execute("SELECT name,sql FROM sqlite_master WHERE type='trigger'").fetchall()
            for name, _ in triggers:
                db.execute('DROP TRIGGER ' + ident(name))
            for raw in src:
                obj = json.loads(raw)
                if obj.get('end'):
                    if obj['counts'] != counts or obj['sha256'] != digest.hexdigest() or src.read():
                        raise ValueError('incomplete, modified or trailing archive')
                    ended = True
                    break
                digest.update(raw.encode())
                table, row = obj['table'], obj['row']
                if table not in meta:
                    raise ValueError('unknown archive table')
                columns = {c[1] for c in db.execute('PRAGMA table_info(' + ident(table) + ')')}
                types = {c['name']: c['type'] for c in meta[table]}
                if table == 'search_docs' and header['backend'] == 'postgres':
                    for generated in ('tsv', 'fts_en', 'fts_pt'):
                        row.pop(generated, None)
                        types.pop(generated, None) # PostgreSQL generated index; rebuilt as FTS5 below.
                if set(row) != set(types) or not set(row) <= columns:
                    raise ValueError('unknown or missing row columns: ' + table)
                row = {k: convert(v, types[k]) for k,v in row.items()}
                affinities = {c[1]:c[2] for c in db.execute('PRAGMA table_info(' + ident(table) + ')')}
                for name,value in row.items():
                    if value is None:
                        continue
                    affinity=affinities[name]
                    valid = {'TEXT': isinstance(value,str), 'BLOB': isinstance(value,bytes), 'INTEGER': isinstance(value,(int,bool)), 'REAL': isinstance(value,(int,float)) and not isinstance(value,bool) and math.isfinite(value)}.get(affinity,False)
                    if not valid:
                        raise ValueError('row value type differs: ' + table + '.' + name)
                    if table=='search_docs' and name=='embedding' and (len(value)!=6144 or not all(math.isfinite(v) for v in struct.unpack('<1536f',value))):
                        raise ValueError('invalid stored embedding')
                if table == 'tape_events' and header['backend'] == 'postgres':
                    row.update(hash_payload=obj['hash_payload'], hash_version=1)
                db.execute('INSERT INTO ' + ident(table) + '(' + ','.join(map(ident,row)) + ') VALUES (' + ','.join('?' for _ in row) + ')', list(row.values()))
                counts[table] += 1
            if not ended:
                raise ValueError('archive missing completion footer')
            if db.execute('PRAGMA foreign_key_check').fetchall():
                raise ValueError('foreign key check failed')
            verify_tape(db)
            if db.execute('PRAGMA integrity_check').fetchone()[0] != 'ok':
                raise ValueError('integrity check failed')
            for table, count in counts.items():
                if db.execute('SELECT count(*) FROM ' + ident(table)).fetchone()[0] != count:
                    raise ValueError('row count mismatch: ' + table)
            if 'search_docs' in actual:
                db.execute("INSERT INTO search_docs_fts(search_docs_fts) VALUES ('rebuild')")
            for _, definition in triggers:
                db.execute(definition)
            if schema == SCHEMA:
                db.execute("CREATE TABLE IF NOT EXISTS _sqlx_migrations (version BIGINT PRIMARY KEY, description TEXT NOT NULL, installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP, success BOOLEAN NOT NULL, checksum BLOB NOT NULL, execution_time BIGINT NOT NULL)")
                db.execute('INSERT INTO _sqlx_migrations(version,description,success,checksum,execution_time) VALUES(1,?,?,?,?)', ('init', True, hashlib.sha384(Path(schema).read_bytes()).digest(), 0))
            db.commit()
        # Link instead of rename: an independently created destination must never be replaced.
        sync_file(tmp)
        os.link(tmp, dest)
        sync_directory(dest.parent)
    finally:
        Path(tmp).unlink(missing_ok=True)


def sync_file(path):
    with open(path, 'rb') as file:
        os.fsync(file.fileno())


def sync_directory(path):
    descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def backup(source, destination):
    if Path(destination).exists():
        raise ValueError('backup destination already exists')
    with private_new(destination):
        pass
    try:
        with sqlite3.connect(f'file:{Path(source).resolve()}?mode=ro', uri=True) as src, sqlite3.connect(destination) as dst:
            src.backup(dst)
            if dst.execute('PRAGMA integrity_check').fetchone()[0] != 'ok':
                raise ValueError('backup integrity check failed')
        sync_file(destination)
        sync_directory(Path(destination).parent)
    except BaseException:
        Path(destination).unlink(missing_ok=True)
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=['export', 'export-postgres', 'restore', 'backup'])
    parser.add_argument('source', help='SQLite path, or PostgreSQL output archive for export-postgres')
    parser.add_argument('destination', nargs='?')
    parser.add_argument('--pg-url-env', help='environment variable containing a PostgreSQL URL; never pass the URL in arguments')
    parser.add_argument('--psql-command', default='psql', help='psql command, including container exec if needed; credentials belong in environment')
    args = parser.parse_args()
    if args.action == 'export-postgres':
        if args.pg_url_env:
            url = urlsplit(os.environ[args.pg_url_env])
            if url.scheme not in ('postgres','postgresql'):
                raise ValueError('PostgreSQL URL required')
            options={'sslmode':'PGSSLMODE','sslrootcert':'PGSSLROOTCERT','sslcert':'PGSSLCERT','sslkey':'PGSSLKEY','connect_timeout':'PGCONNECT_TIMEOUT','target_session_attrs':'PGTARGETSESSIONATTRS','application_name':'PGAPPNAME','options':'PGOPTIONS'}
            for key,value in parse_qsl(url.query):
                if key not in options:
                    raise ValueError('unsupported PostgreSQL URL option')
                os.environ[options[key]]=value
            for key,value in {'PGHOST':url.hostname,'PGPORT':str(url.port or 5432),'PGUSER':unquote(url.username or ''),'PGPASSWORD':unquote(url.password or ''),'PGDATABASE':unquote(url.path.lstrip('/'))}.items():
                os.environ[key] = value or ''
        postgres_export(args.source, shlex.split(args.psql_command))
    elif not args.destination:
        parser.error('destination required')
    else:
        {'export': sqlite_export, 'restore': restore, 'backup': backup}[args.action](args.source, args.destination)


if __name__ == '__main__':
    main()
