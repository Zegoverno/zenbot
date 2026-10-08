#!/usr/bin/env python3
"""OpenRouter System One stand-in: checks auth and the bool→noul wire contract."""
import json
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer


class H(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        try:
            assert self.path == "/systemone"
            assert self.headers.get("Authorization") == "Bearer e2e-key"
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            assert request["model"] == "typesafe/jev-1.13"
            answers = {}
            for name, question in request["questions"].items():
                kind = question["type"]
                if kind == "noul":
                    answers[name] = {"type": "noul", "noul": 0.82}
                elif kind == "choice":
                    choice = next(iter(question["criteria"]))
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
        except (AssertionError, KeyError, ValueError) as error:
            status, payload = 400, {"error": str(error)}
        body = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


HTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
