#!/usr/bin/env python3
"""Archive safety/roundtrip checks, including legacy PostgreSQL canonical tape hashes."""
import hashlib
import json
from pathlib import Path
import sqlite3
import tempfile
import unittest
import uuid
import storage


class StorageTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.db = self.root / 'source.sqlite3'
        with sqlite3.connect(self.db) as db:
            db.executescript(storage.SCHEMA.read_text())
            sid = uuid.uuid4().bytes
            db.execute("INSERT INTO sessions(id,model,title) VALUES(?, 'faux/smoke','日本語')", (sid,))
            payload = '{"text": "hello", "count": 1}'
            digest = hashlib.sha256(('user' + payload).encode()).hexdigest()
            db.execute('INSERT INTO tape_events(session_id,kind,payload,seq,parent,hash,hash_payload,hash_version) VALUES(?,?,?,?,?,?,?,?)', (sid,'user',payload,1,None,digest,payload,1))

    def tearDown(self):
        self.temp.cleanup()

    def test_roundtrip_preserves_rows_and_private_files(self):
        archive, restored = self.root/'state.jsonl', self.root/'restored'
        storage.sqlite_export(self.db, archive)
        storage.restore(archive, restored)
        self.assertEqual(archive.stat().st_mode & 0o777, 0o600)
        self.assertEqual(restored.stat().st_mode & 0o777, 0o600)
        with sqlite3.connect(self.db) as old, sqlite3.connect(restored) as new:
            for table in storage.tables(old):
                self.assertEqual(old.execute('SELECT * FROM '+storage.ident(table)).fetchall(), new.execute('SELECT * FROM '+storage.ident(table)).fetchall(), table)
            self.assertEqual(new.execute('SELECT version FROM _sqlx_migrations').fetchone(), (1,))
            self.assertTrue(new.execute("SELECT count(*) FROM sqlite_master WHERE type='trigger'").fetchone()[0])

    def test_incomplete_or_modified_archive_never_publishes(self):
        archive = self.root/'state.jsonl'
        storage.sqlite_export(self.db, archive)
        original = archive.read_text()
        for bad in [original.rsplit('\n',2)[0]+'\n',original.replace('日本語','changed'),original+'{}\n']:
            archive.write_text(bad)
            with self.assertRaises((ValueError,KeyError,json.JSONDecodeError)):
                storage.restore(archive,self.root/'new')
            self.assertFalse((self.root/'new').exists())
        self.assertEqual(list(self.root.glob('new.*')),[])

    def test_refuses_destination_overwrite(self):
        archive = self.root/'state.jsonl'
        storage.sqlite_export(self.db, archive)
        with self.assertRaises(ValueError):
            storage.restore(archive,self.db)
        with self.assertRaises(ValueError):
            storage.backup(self.db,self.db)

    def test_backup_includes_live_wal(self):
        with sqlite3.connect(self.db) as db:
            db.execute('PRAGMA journal_mode=WAL')
            db.execute("UPDATE sessions SET title='committed WAL'")
            db.commit()
            storage.backup(self.db,self.root/'backup')
            with sqlite3.connect(self.root/'backup') as copy:
                self.assertEqual(copy.execute('SELECT title FROM sessions').fetchone()[0],'committed WAL')

    def test_typed_pg_values(self):
        value=uuid.uuid4()
        self.assertEqual(storage.convert(str(value),'uuid'),value.bytes)
        self.assertEqual(storage.convert('\\x00ff','bytea'),b'\x00\xff')
        self.assertEqual(len(storage.convert('[0.1,'+','.join(['0']*1535)+']','vector')),6144)
        with self.assertRaises(ValueError):
            storage.convert('[1,2]','vector')

    def test_tape_corruption_rejected_even_with_valid_archive_checksum(self):
        with sqlite3.connect(self.db) as db:
            db.execute("UPDATE tape_events SET hash='bad'")
        storage.sqlite_export(self.db,self.root/'archive')
        with self.assertRaisesRegex(ValueError,'hash mismatch'):
            storage.restore(self.root/'archive',self.root/'new')

    def test_export_never_removes_existing_archive(self):
        archive = self.root / 'existing.jsonl'
        original = b'keep this private archive intact\n'
        archive.write_bytes(original)
        with self.assertRaises(FileExistsError):
            storage.sqlite_export(self.db, archive)
        self.assertEqual(archive.read_bytes(), original)

    def test_pg_json_scalars_distinguish_sql_null(self):
        # JSON columns are exported as PostgreSQL canonical text, preserving SQL
        # NULL separately from a JSON null and preserving every scalar type.
        for canonical in ['"quoted text"', 'true', 'false', '17', '1.25', 'null', '{"nested": null}', '[1, true]']:
            for typ in ['json', 'jsonb']:
                self.assertEqual(storage.convert(canonical, typ), canonical)
        self.assertIsNone(storage.convert(None, 'jsonb'))
        self.assertEqual(storage.convert('null', 'jsonb'), 'null')

    def test_changed_payload_with_unchanged_hash_bytes_is_rejected(self):
        # An attacker can recompute the unsigned archive footer, so tape-domain
        # validation must also check that hashed bytes describe the stored payload.
        with sqlite3.connect(self.db) as db:
            db.execute("UPDATE tape_events SET payload=?", ('{"text":"different","count":1}',))
        archive = self.root / 'changed-payload.jsonl'
        storage.sqlite_export(self.db, archive)
        with self.assertRaisesRegex(ValueError, 'payload'):
            storage.restore(archive, self.root / 'new')
        self.assertFalse((self.root / 'new').exists())

    def test_unknown_hash_version_rejected(self):
        # Schema constraints also prevent unsupported hash versions before restore.
        with sqlite3.connect(self.db) as db:
            with self.assertRaises(sqlite3.IntegrityError):
                db.execute('UPDATE tape_events SET hash_version=99')

    def test_denial_checks_bound_work_to_dependency_graph(self):
        # A long lineage with one denied leaf has one affected session. Avoid
        # computing all ancestor pairs: that grows quadratically with history.
        with sqlite3.connect(self.db) as db:
            previous = None
            ids = []
            for _ in range(1800):
                current = uuid.uuid4().bytes
                db.execute("INSERT INTO sessions(id,model,parent) VALUES(?,'faux/smoke',?)", (current, previous))
                ids.append(current)
                previous = current
            db.execute('INSERT INTO contamination(cause,affected) VALUES(?,?)', (ids[-1], ids[-1]))
            steps = [0]

            def budget():
                steps[0] += 1
                return steps[0] > 1000

            # Budget SQLite VM instructions, not elapsed time, to keep the test
            # deterministic on shared CI. Exhausting it aborts the query.
            db.set_progress_handler(budget, 1000)
            self.assertEqual(db.execute('SELECT id FROM zen_contaminated_sessions').fetchall(), [(ids[-1],)])
            steps[0] = 0
            # A new dependency closes a cycle: every chain member is affected,
            # but each ID is visited once and the traversal still terminates.
            db.execute('INSERT INTO session_inputs(reader,writer) VALUES(?,?)', (ids[0], ids[-1]))
            actual = {row[0] for row in db.execute('SELECT id FROM zen_contaminated_sessions')}
            self.assertEqual(actual, set(ids))
            steps[0] = 0
            db.execute("UPDATE contamination SET cleared_at='2026-10-22T00:00:00+00:00'")
            self.assertEqual(db.execute('SELECT count(*) FROM zen_contaminated_sessions').fetchone(), (0,))
            db.set_progress_handler(None, 0)


if __name__ == '__main__':
    unittest.main()
