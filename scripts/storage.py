#!/usr/bin/env python3
"""Online backup of the SQLite state file: storage.py backup <live.sqlite3> <backup.sqlite3>.

Uses SQLite's backup API, so a running kernel's WAL is included. The backup is private and
integrity-checked; a failed one leaves nothing behind.
"""
import os
from pathlib import Path
import sqlite3
import sys


def backup(source, destination):
    dest = Path(destination)
    if dest.exists():
        raise SystemExit('backup destination already exists')
    os.close(os.open(dest, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600))
    try:
        with sqlite3.connect(f'file:{Path(source).resolve()}?mode=ro', uri=True) as src, sqlite3.connect(dest) as dst:
            src.backup(dst)
            if dst.execute('PRAGMA integrity_check').fetchone()[0] != 'ok':
                raise SystemExit('backup integrity check failed')
        with open(dest, 'rb') as f:
            os.fsync(f.fileno())
    except BaseException:
        dest.unlink(missing_ok=True)
        raise


if __name__ == '__main__':
    if len(sys.argv) != 4 or sys.argv[1] != 'backup':
        raise SystemExit(__doc__)
    backup(sys.argv[2], sys.argv[3])
