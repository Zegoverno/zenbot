#!/usr/bin/env python3
"""Execute explicit SQLite SQL for development/test assertions; no PostgreSQL translation."""
import sqlite3
import sys
import uuid

db = sqlite3.connect(sys.argv[1], timeout=30)
db.execute('PRAGMA foreign_keys=ON')
buffer = ''
for char in sys.argv[2]:
    buffer += char
    if char == ';' and sqlite3.complete_statement(buffer):
        cursor = db.execute(buffer)
        if cursor.description:
            for row in cursor:
                print('|'.join(str(uuid.UUID(bytes=v)) if isinstance(v,bytes) and len(v)==16 else '' if v is None else str(v) for v in row))
        buffer = ''
if buffer.strip():
    for row in db.execute(buffer):
        print('|'.join(str(uuid.UUID(bytes=v)) if isinstance(v,bytes) and len(v)==16 else '' if v is None else str(v) for v in row))
db.commit()
