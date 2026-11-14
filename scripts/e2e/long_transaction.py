#!/usr/bin/env python3
"""Hold a SQLite write transaction while a competing fixture writer waits."""
import sqlite3
import sys
import time
with sqlite3.connect(sys.argv[1],timeout=30) as db:
    db.execute('BEGIN IMMEDIATE')
    db.execute("UPDATE memories SET tier='archived' WHERE text LIKE 'LONGTX%'")
    time.sleep(3)
