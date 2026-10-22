#!/usr/bin/env python3
"""Independently verify tape chains, or append a deterministic historical fixture block."""
import hashlib
import json
from pathlib import Path
import sqlite3
import sys
import uuid
sys.path.insert(0,str(Path(__file__).resolve().parents[1]))
import storage
with sqlite3.connect(sys.argv[2],timeout=30) as db:
    db.execute('PRAGMA foreign_keys=ON')
    if sys.argv[1]=='verify':
        storage.verify_tape(db)
    else:
        sid=uuid.UUID(sys.argv[3]).bytes
        payload=json.dumps(json.loads(sys.argv[4]),ensure_ascii=False,separators=(',',':'))
        db.execute('BEGIN IMMEDIATE')
        prev=db.execute('SELECT id,seq,hash FROM tape_events WHERE session_id=? ORDER BY seq DESC LIMIT 1',(sid,)).fetchone()
        parent,seq,ph=prev if prev else (None,0,'')
        digest=hashlib.sha256((ph+'message'+payload).encode()).hexdigest()
        db.execute("INSERT INTO tape_events(session_id,kind,payload,seq,parent,hash,hash_payload,hash_version,created_at) VALUES(?,'message',?,?,?,?,?,2,strftime('%Y-%m-%dT%H:%M:%f+00:00','now','-1 minute'))",(sid,payload,seq+1,parent,digest,payload))
