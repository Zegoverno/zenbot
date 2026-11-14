#!/usr/bin/env python3
"""Independently verify tape chains, or append a deterministic historical fixture block."""
import hashlib
import json
import sqlite3
import sys
import uuid


def verify_tape(db):
    previous = {}
    for sid, event_id, seq, parent, kind, payload, stored, actual_payload, version in db.execute('SELECT session_id,id,seq,parent,kind,hash_payload,hash,payload,hash_version FROM tape_events ORDER BY session_id,seq'):
        prev_id, prev_hash, prev_seq = previous.get(sid, (None, '', 0))
        if parent != prev_id or seq != prev_seq + 1 or payload is None:
            raise ValueError('invalid tape chain linkage or missing canonical payload')
        if version not in (1, 2) or json.dumps(json.loads(actual_payload),sort_keys=True,separators=(',',':')) != json.dumps(json.loads(payload),sort_keys=True,separators=(',',':')):
            raise ValueError('tape payload differs from hashed payload or unknown hash format')
        if hashlib.sha256((prev_hash + kind + payload).encode()).hexdigest() != stored:
            raise ValueError('tape hash mismatch')
        previous[sid] = (event_id, stored, seq)


with sqlite3.connect(sys.argv[2],timeout=30) as db:
    db.execute('PRAGMA foreign_keys=ON')
    if sys.argv[1]=='verify':
        verify_tape(db)
    else:
        sid=uuid.UUID(sys.argv[3]).bytes
        payload=json.dumps(json.loads(sys.argv[4]),ensure_ascii=False,separators=(',',':'))
        db.execute('BEGIN IMMEDIATE')
        prev=db.execute('SELECT id,seq,hash FROM tape_events WHERE session_id=? ORDER BY seq DESC LIMIT 1',(sid,)).fetchone()
        parent,seq,ph=prev if prev else (None,0,'')
        digest=hashlib.sha256((ph+'message'+payload).encode()).hexdigest()
        db.execute("INSERT INTO tape_events(session_id,kind,payload,seq,parent,hash,hash_payload,hash_version,created_at) VALUES(?,'message',?,?,?,?,?,2,strftime('%Y-%m-%dT%H:%M:%f+00:00','now','-1 minute'))",(sid,payload,seq+1,parent,digest,payload))
