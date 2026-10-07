#!/usr/bin/env python3
"""A tiny MCP server for the e2e tests (scripts/e2e.sh): two tools, `echo` and `add`.

stdio by default (one JSON-RPC message per line); `--http PORT` serves streamable HTTP on
127.0.0.1:PORT/mcp instead, answering tools/call as an event stream (text/event-stream) and
everything else as JSON, with an Mcp-Session-Id, so both answer kinds are exercised.
"""
import json
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

TOOLS = [
    {"name": "echo", "description": "Echo a message back.\nSecond line of the description.",
     "inputSchema": {"type": "object", "properties": {"message": {"type": "string"}}, "required": ["message"]}},
    {"name": "add", "description": "Add two numbers.",
     "inputSchema": {"type": "object", "properties": {"a": {"type": "number"}, "b": {"type": "number"}}, "required": ["a", "b"]}},
]


def handle(msg):
    """The response to a request, or None for a notification."""
    if "id" not in msg:
        return None
    method, params = msg.get("method"), msg.get("params") or {}
    if method == "initialize":
        result = {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}}, "serverInfo": {"name": "e2e", "version": "1"}}
    elif method == "tools/list":
        result = {"tools": TOOLS}
    elif method == "tools/call":
        args = params.get("arguments") or {}
        if params.get("name") == "echo":
            result = {"content": [{"type": "text", "text": "echo: " + str(args.get("message"))}]}
        elif params.get("name") == "add":
            result = {"content": [{"type": "text", "text": str(args["a"] + args["b"])}]}
        else:
            return {"jsonrpc": "2.0", "id": msg["id"], "error": {"code": -32602, "message": "unknown tool"}}
    else:
        return {"jsonrpc": "2.0", "id": msg["id"], "error": {"code": -32601, "message": "unknown method"}}
    return {"jsonrpc": "2.0", "id": msg["id"], "result": result}


class Http(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def do_POST(self):
        msg = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))))
        resp = handle(msg)
        if resp is None:
            self.send_response(202)
            self.end_headers()
            return
        stream = msg.get("method") == "tools/call"
        body = ("event: message\ndata: " + json.dumps(resp) + "\n\n") if stream else json.dumps(resp)
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream" if stream else "application/json")
        self.send_header("Mcp-Session-Id", "e2e-session")
        self.send_header("Content-Length", str(len(body.encode())))
        self.end_headers()
        self.wfile.write(body.encode())


if __name__ == "__main__":
    if len(sys.argv) > 2 and sys.argv[1] == "--http":
        HTTPServer(("127.0.0.1", int(sys.argv[2])), Http).serve_forever()
    for line in sys.stdin:
        if line.strip():
            out = handle(json.loads(line))
            if out is not None:
                print(json.dumps(out), flush=True)
