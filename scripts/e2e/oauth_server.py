#!/usr/bin/env python3
"""An MCP server behind OAuth, for the e2e `mcp-login` scenario (scripts/e2e.sh, D-072).

One HTTP server on 127.0.0.1:PORT plays both parts:
- the protected MCP endpoint `/mcp`: without a valid bearer token it answers 401 with
  `WWW-Authenticate: Bearer resource_metadata=…, scope="tools"`; with one it serves `initialize`,
  `tools/list` (one tool, `whoami`) and `tools/call` (`whoami` answers "token <n>");
- the authorization server `/auth`: protected resource and authorization server metadata, dynamic
  registration (loopback redirect only), `/auth/authorize` (answers the browser's redirect at once
  with code, state and iss: the "approve" step), and `/auth/token` (authorization code with PKCE
  S256 and the resource indicator; refresh tokens rotate, an old one is refused).

Access tokens live EXPIRES seconds (default 1), so every later call makes the client refresh.
`/stats` reports registrations, refreshes and the last access token's number.
"""
import base64
import hashlib
import json
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlencode, urlparse

PORT = int(sys.argv[1])
EXPIRES = int(sys.argv[2]) if len(sys.argv) > 2 else 1
BASE = f"http://127.0.0.1:{PORT}"
RESOURCE = f"{BASE}/mcp"
ISSUER = f"{BASE}/auth"
REDIRECT = "http://127.0.0.1:33418/callback"

lock = threading.Lock()
clients, codes, access, refresh = set(), {}, {}, set()
stats = {"registrations": 0, "refreshes": 0, "last": 0, "count": 0}


def issue():
    stats["count"] += 1
    n = stats["count"]
    access[f"at-{n}"] = time.time() + EXPIRES + 30  # the client refreshes 30 s early
    refresh.add(f"rt-{n}")
    stats["last"] = n
    return {"access_token": f"at-{n}", "token_type": "Bearer", "expires_in": EXPIRES, "refresh_token": f"rt-{n}", "scope": "tools"}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def send(self, code, body=None, headers=()):
        data = json.dumps(body).encode() if body is not None else b""
        self.send_response(code)
        for k, v in headers:
            self.send_header(k, v)
        if body is not None:
            self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def body(self):
        return self.rfile.read(int(self.headers.get("Content-Length") or 0))

    def do_GET(self):
        url = urlparse(self.path)
        if url.path in ("/.well-known/oauth-protected-resource/mcp", "/.well-known/oauth-protected-resource"):
            return self.send(200, {"resource": RESOURCE, "authorization_servers": [ISSUER], "scopes_supported": ["tools"]})
        if url.path == "/.well-known/oauth-authorization-server/auth":
            return self.send(200, {"issuer": ISSUER, "authorization_endpoint": f"{ISSUER}/authorize", "token_endpoint": f"{ISSUER}/token",
                                   "registration_endpoint": f"{ISSUER}/register", "code_challenge_methods_supported": ["S256"],
                                   "authorization_response_iss_parameter_supported": True})
        if url.path == "/auth/authorize":
            q = {k: v[0] for k, v in parse_qs(url.query).items()}
            ok = (q.get("client_id") in clients and q.get("redirect_uri") == REDIRECT and q.get("code_challenge_method") == "S256"
                  and q.get("resource") == RESOURCE and q.get("response_type") == "code" and q.get("scope") == "tools")
            if not ok:
                return self.send(400, {"error": "invalid_request", "got": q})
            with lock:
                code = f"code-{len(codes) + 1}"
                codes[code] = (q["client_id"], q["code_challenge"])
            return self.send(302, None, [("Location", REDIRECT + "?" + urlencode({"code": code, "state": q["state"], "iss": ISSUER}))])
        if url.path == "/stats":
            return self.send(200, stats)
        self.send(404, {"error": "not found"})

    def do_POST(self):
        url = urlparse(self.path)
        raw = self.body()
        if url.path == "/auth/register":
            req = json.loads(raw)
            if req.get("redirect_uris") != [REDIRECT] or req.get("token_endpoint_auth_method") != "none":
                return self.send(400, {"error": "invalid_redirect_uri"})
            with lock:
                stats["registrations"] += 1
                cid = f"client-{stats['registrations']}"
                clients.add(cid)
            return self.send(201, {"client_id": cid, "redirect_uris": [REDIRECT]})
        if url.path == "/auth/token":
            f = {k: v[0] for k, v in parse_qs(raw.decode()).items()}
            if f.get("resource") != RESOURCE:
                return self.send(400, {"error": "invalid_target"})
            with lock:
                if f.get("grant_type") == "authorization_code":
                    client, challenge = codes.pop(f.get("code", ""), (None, None))
                    verifier = f.get("code_verifier", "")
                    digest = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).rstrip(b"=").decode()
                    if client is None or client != f.get("client_id") or digest != challenge or f.get("redirect_uri") != REDIRECT:
                        return self.send(400, {"error": "invalid_grant"})
                    return self.send(200, issue())
                if f.get("grant_type") == "refresh_token":
                    rt = f.get("refresh_token", "")
                    if rt not in refresh:
                        return self.send(400, {"error": "invalid_grant"})
                    refresh.discard(rt)
                    stats["refreshes"] += 1
                    return self.send(200, issue())
            return self.send(400, {"error": "unsupported_grant_type"})
        if url.path == "/mcp":
            token = (self.headers.get("Authorization") or "").removeprefix("Bearer ")
            if access.get(token, 0) < time.time():
                return self.send(401, {"error": "unauthorized"}, [("WWW-Authenticate",
                    f'Bearer error="invalid_token", resource_metadata="{BASE}/.well-known/oauth-protected-resource/mcp", scope="tools"')])
            msg = json.loads(raw)
            if "id" not in msg:
                return self.send(202)
            method = msg.get("method")
            if method == "initialize":
                result = {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}}, "serverInfo": {"name": "oauth-e2e", "version": "1"}}
            elif method == "tools/list":
                result = {"tools": [{"name": "whoami", "description": "Say which token called.", "inputSchema": {"type": "object", "properties": {}}}]}
            elif method == "tools/call":
                result = {"content": [{"type": "text", "text": f"token {token.removeprefix('at-')}"}]}
            else:
                return self.send(200, {"jsonrpc": "2.0", "id": msg["id"], "error": {"code": -32601, "message": "no"}})
            return self.send(200, {"jsonrpc": "2.0", "id": msg["id"], "result": result})
        self.send(404, {"error": "not found"})


ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
