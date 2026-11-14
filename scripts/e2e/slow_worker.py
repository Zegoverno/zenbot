#!/usr/bin/env python3
"""A worker for the e2e tests that serves one model, `slow/summarizer`, whose `complete` takes
ZEN_SLOW_SECS (default 12) seconds: a summarizer slower than the watchdog's idle limit.
Returns a valid summary keeping every line marked FACT:, like the faux engine's."""
import json, os, sys, threading, time

lock = threading.Lock()


def send(msg):
    with lock:
        sys.stdout.write(json.dumps(msg) + "\n")
        sys.stdout.flush()


def complete(req_id, prompt):
    time.sleep(float(os.environ.get("ZEN_SLOW_SECS", "12")))
    facts = [{"text": l[l.index("FACT:"):].rstrip('"\\'), "refs": []} for l in prompt.splitlines() if "FACT:" in l]
    summary = {"goal": "(slow scripted summary)", "state": "", "decisions": [], "files": [], "facts": facts, "open": [], "next": ""}
    send({"jsonrpc": "2.0", "id": req_id, "result": {"text": json.dumps(summary), "usage": {}, "model": "slow/summarizer"}})


for line in sys.stdin:
    msg = json.loads(line)
    method, params, req_id = msg.get("method"), msg.get("params") or {}, msg.get("id")
    if method == "complete":
        threading.Thread(target=complete, args=(req_id, params.get("prompt", "")), daemon=True).start()
        continue
    result = {
        "ping": {"pong": True},
        "models.list": {"authenticated": {"slow": True}, "models": [{"id": "slow/summarizer", "name": "Slow summarizer (test)"}]},
    }.get(method, {"ok": True})
    if req_id is not None:
        send({"jsonrpc": "2.0", "id": req_id, "result": result})
