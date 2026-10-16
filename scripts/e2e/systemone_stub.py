#!/usr/bin/env python3
"""OpenRouter System One stand-in: checks auth and the bool→noul wire contract.

Usage: systemone_stub.py <port> [release-file]. A sleep question about an entry marked HOLD waits
until the release file exists (at most 30 s), so a scenario can change memory mid-sleep. GET /stats
reports how many calls are held now and the most sleep questions (a state with a `memory`) that
were in flight at once; other calls (a `remember` turn's) aren't counted.
"""
import json
import os
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

RELEASE = sys.argv[2] if len(sys.argv) > 2 else None
LOCK = threading.Lock()
STATS = {"inflight": 0, "max_inflight": 0, "held": 0}


def covered_by(question, state):
    """The sleep's cover choice: a DUPLICATE entry is covered by another DUPLICATE one, a
    PROFILE-COPY entry by the owner's profile, anything else by nothing."""
    memory = str(state.get("memory", ""))
    others = state.get("other_memories", {})
    if "DUPLICATE" in memory:
        return next(k for k, text in others.items() if "DUPLICATE" in text)
    if "PROFILE-COPY" in memory:
        return "user_profile"
    assert "none" in question["criteria"]
    return "none"


class H(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def reply(self, status, payload):
        body = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        # Complete the measured work before publishing its response: the client can
        # start its next request as soon as it receives this body.
        self.finish_counted_request()
        self.wfile.write(body)

    def finish_counted_request(self):
        if getattr(self, "counted", False):
            with LOCK:
                STATS["inflight"] -= 1
                self.counted = False

    def do_GET(self):
        with LOCK:
            self.reply(200, dict(STATS))

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        sleep = isinstance(request.get("state"), dict) and "memory" in request["state"]
        if sleep:
            with LOCK:
                self.counted = True
                STATS["inflight"] += 1
                STATS["max_inflight"] = max(STATS["max_inflight"], STATS["inflight"])
        try:
            self.answer(request)
        finally:
            self.finish_counted_request()

    def answer(self, request):
        try:
            assert self.path == "/systemone"
            assert self.headers.get("Authorization") == "Bearer e2e-key"
            assert request["model"] == "typesafe/jev-1.13"
            state = request.get("state", {})
            # The job gate: the task the agent wrote comes as untrusted data, never as evidence.
            if "job" in state:
                assert "task" not in state["job"]
                assert state["job"]["task_written_by_agent"].startswith('<untrusted source="agent"')
            answers = {}
            source_batch = any(n == "writers" or n.endswith("_writers") for n in request["questions"])
            if source_batch:
                entries = [state] if "host" in state else list(state.values())
                for entry in entries:
                    assert set(entry) == {"host", "scope", "path", "path_depth", "reached"}
                    assert "?" not in entry["path"]
                if any(e["host"] == "unavailable.source.test" for e in entries):
                    return self.reply(503, {"error": "unavailable fixture"})
            # The sleep's marker entries: lasting and about the owner, or lasting knowledge.
            memory = str(state.get("memory", ""))
            if "HOLD" in memory and RELEASE:
                with LOCK:
                    STATS["held"] += 1
                deadline = time.time() + 30
                while not os.path.exists(RELEASE) and time.time() < deadline:
                    time.sleep(0.05)
                with LOCK:
                    STATS["held"] -= 1
            high = {"PROMOTE-USER": {"durable", "about_owner"}, "PROMOTE-WIKI": {"durable", "knowledge"}}
            marked = next((v for k, v in high.items() if k in memory), None)
            for name, question in request["questions"].items():
                kind = question["type"]
                if source_batch:
                    entry = state if "host" in state else state[name.split("_", 1)[0]]
                    field = name.rsplit("_", 1)[-1]
                    if field == "scope":
                        choice = "page" if "/issues" in entry["path"] else ("surface" if entry["path_depth"] >= 2 else "account")
                        answers[name] = {"type": "choice", "choice": choice, "probabilities": {choice: 1.0}}
                    elif field == "platform":
                        answers[name] = {"type": "noul", "noul": 1.0}
                    else:
                        known = entry["host"] == "reputable.source.test"
                        choice = ("anyone" if "/issues" in entry["path"] else "organisation") if field == "writers" else "reputable"
                        if not known:
                            choice = "unknown"
                        answers[name] = {"type": "choice", "choice": choice, "probabilities": {choice: 1.0}}
                elif name == "alignment":
                    assert set(state) == {"job_data", "proposed_action_UNTRUSTED_DATA", "action_class"}
                    goal = state["job_data"]["authenticated_goal_data"]
                    assert "LATE_POISON_PLAN" not in state["job_data"]["pre_exposure_brief_plan_data"]
                    if "ALIGN-UNAVAILABLE" in goal:
                        return self.reply(503, {"error": "unavailable alignment"})
                    if "ALIGN-MALFORMED" in goal:
                        return self.reply(200, {"answers": {"alignment": {"choice": "nonsense"}}})
                    action = state["proposed_action_UNTRUSTED_DATA"]
                    choice = "contradicts" if "ALIGN-CONTRADICTS" in goal else "follows"
                    if action["tool"] == "schedule" or action["arguments"].get("command") == "git push":
                        choice = "contradicts" if "ALIGN-CONTRADICTS" in goal else "unrelated"
                    answers[name] = {"type": "choice", "choice": choice, "probabilities": {choice: 1.0}}
                elif name == "injection":
                    assert set(state) == {"untrusted_text"}
                    answers[name] = {"type": "noul", "noul": 1.0 if "POISON" in state["untrusted_text"] else 0.0}
                elif name in ("known", "sensitive") and "pages" in state:
                    # Wiki fixtures are new facts and contain only an already masked token.
                    answers[name] = {"type": "noul", "noul": 0.0}
                elif name == "kind" and "task" in state:
                    choice = "build"
                    assert choice in question["criteria"]
                    answers[name] = {"type": "choice", "choice": choice, "probabilities": {choice: 1.0}}
                elif kind == "noul" and marked is not None:
                    answers[name] = {"type": "noul", "noul": 0.97 if name in marked else 0.05}
                elif kind == "noul":
                    answers[name] = {"type": "noul", "noul": 0.82}
                elif kind == "choice":
                    choice = covered_by(question, state) if name == "covered_by" else ("none" if name == "same" else next(iter(question["criteria"])))
                    answers[name] = {"type": "choice", "choice": choice,
                                     "probabilities": {choice: 1.0}, "confidence": 1.0}
                elif kind == "score":
                    answers[name] = {"type": "score", "score": 1,
                                     "probabilities": {"1": 1.0}, "confidence": 1.0}
                else:
                    raise ValueError(f"wrong question type {kind}")
            status = 200
            payload = {"model": "typesafe/jev-1.13-test", "provider": "e2e",
                       "answers": answers, "usage": {"input_tokens": 100,
                       "output_tokens": 10, "cost": 0.00001}}
        except (AssertionError, KeyError, ValueError, StopIteration) as error:
            status, payload = 400, {"error": str(error)}
        self.reply(status, payload)


ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
