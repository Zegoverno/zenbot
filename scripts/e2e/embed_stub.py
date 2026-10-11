#!/usr/bin/env python3
"""OpenAI-compatible /embeddings stand-in for the search e2e.

Usage: embed_stub.py <port> [release-file]. A text's embedding counts its words, each in a
dimension of its own (md5 of the word), with a few synonyms sharing one, so a query can match a
document by meaning without sharing a word. Values are whole numbers, so the kernel and the
scenario's reference query (search_reference.sql) build the same vectors. A batch with a text
containing REJECT-EMBED is refused with 400, as a provider refuses an input; a batch with
SLOW-EMBED waits until the release file exists (at most 30 s). GET /stats reports the batches
served, the texts embedded and how many calls are held now.
"""
import hashlib
import json
import os
import re
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

RELEASE = sys.argv[2] if len(sys.argv) > 2 else None
LOCK = threading.Lock()
STATS = {"batches": 0, "texts": 0, "held": 0, "rejected": 0}
SAME = {"automobile": "car", "vehicle": "car", "sedan": "car"}


def vector(text):
    v = [0] * 1536
    for w in re.findall(r"[a-z0-9]+", text.lower()):
        w = SAME.get(w, w)
        v[int(hashlib.md5(w.encode()).hexdigest(), 16) % 1536] += 1
    if not any(v):
        v[0] = 1
    return v


class H(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def reply(self, status, payload):
        body = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        with LOCK:
            self.reply(200, dict(STATS))

    def do_POST(self):
        req = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))))
        if self.headers.get("Authorization") != "Bearer e2e-key" or req.get("dimensions") != 1536:
            return self.reply(401, {"error": {"message": "bad request"}})
        texts = req["input"]
        if any("REJECT-EMBED" in t for t in texts):
            with LOCK:
                STATS["rejected"] += 1
            return self.reply(400, {"error": {"message": "input rejected by the e2e stub", "code": 400}})
        if any("SLOW-EMBED" in t for t in texts):
            with LOCK:
                STATS["held"] += 1
            deadline = time.time() + 30
            while RELEASE and not os.path.exists(RELEASE) and time.time() < deadline:
                time.sleep(0.1)
            with LOCK:
                STATS["held"] -= 1
        with LOCK:
            STATS["batches"] += 1
            STATS["texts"] += len(texts)
        self.reply(200, {"data": [{"index": i, "embedding": vector(t)} for i, t in enumerate(texts)]})


ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
