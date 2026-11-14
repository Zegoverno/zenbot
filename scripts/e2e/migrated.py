#!/usr/bin/env python3
"""SQL that records migrations as applied the way sqlx does (its _sqlx_migrations table, a SHA-384
checksum of each file), for the migration files up to a version: an upgrade test applies those
files with psql, then this, and the kernel applies only the rest.

Usage: migrated.py <migrations dir> <last version>"""
import hashlib
import pathlib
import sys

folder, last = pathlib.Path(sys.argv[1]), int(sys.argv[2])
print("""CREATE TABLE IF NOT EXISTS _sqlx_migrations (version BIGINT PRIMARY KEY, description TEXT NOT NULL,
  installed_on TIMESTAMPTZ NOT NULL DEFAULT now(), success BOOLEAN NOT NULL, checksum BYTEA NOT NULL, execution_time BIGINT NOT NULL);""")
for f in sorted(folder.glob('*.sql')):
    version, _, rest = f.stem.partition('_')
    if int(version) <= last:
        checksum = hashlib.sha384(f.read_bytes()).hexdigest()
        print(f"INSERT INTO _sqlx_migrations(version,description,success,checksum,execution_time) VALUES ({int(version)}, '{rest.replace('_', ' ')}', true, decode('{checksum}','hex'), 0);")
