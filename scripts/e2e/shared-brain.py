#!/usr/bin/env python3
"""Real stdio MCP + authenticated faux kernel; only temporary homes, files and SQLite."""
import json, os, pathlib, socket, sqlite3, subprocess, tempfile, time, urllib.request, urllib.error

repo = pathlib.Path(__file__).resolve().parents[2]
binary = pathlib.Path(os.environ.get("ZEN_E2E_BIN", repo / "target/release"))
with tempfile.TemporaryDirectory(prefix="zen-shared-brain-") as tmp:
    root = pathlib.Path(tmp)
    home = root / "home"
    zh = home / ".zenbot"
    wiki = root / "configured-wiki"
    wiki.mkdir(parents=True)
    (wiki / "page.md").write_text("shared knowledge sentinel")
    (root / "outside.md").write_text("outside secret sentinel")
    (wiki / "escape.md").symlink_to(root / "outside.md")
    (wiki / "escape-dir").symlink_to(root, target_is_directory=True)
    for agent in ["alice", "bob"]:
        folder = zh / "agents" / agent
        folder.mkdir(parents=True)
        (folder / "SOUL.md").write_text("test agent")
        (folder / "agent.toml").write_text('global = "none"\n')
        vault = zh / "global/agents" / agent / "wiki"
        vault.mkdir(parents=True)
        (vault / "private.md").write_text(agent + " private wiki sentinel")
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    url = f"http://127.0.0.1:{port}"
    env = {k: v for k, v in os.environ.items() if not (k.startswith("ZEN_") or k in ["DATABASE_URL","OPENROUTER_API_KEY"])}
    env.update(HOME=str(home), ZEN_HOME=str(zh), ZEN_TOKEN="synthetic-mcp-token", ZEN_PORT=str(port),
               ZEN_WORKSPACE=str(root), ZEN_REPO=str(repo), DATABASE_URL="sqlite://" + str(root / "zen.sqlite3"),
               ZEN_DEFAULT_MODEL="faux/smoke", ZEN_WORKERS="engine", ZEN_ENGINE_CMD=str(binary / "zen-engine"),
               ZEN_FAUX="1", ZEN_JOBS="0", ZEN_EMBED="0", ZEN_SUGGEST="0", ZEN_WIKI_DIR=str(wiki),
               ZEN_UPDATE_CHECK_SECS="0", ZEN_REVIEW="0")
    def api(path, body=None, token="synthetic-mcp-token"):
        req = urllib.request.Request(url + path, data=None if body is None else json.dumps(body).encode(),
              headers={"Authorization":"Bearer " + token,"Content-Type":"application/json"})
        with urllib.request.urlopen(req, timeout=10) as response:
            return json.load(response)
    log = open(root / "kernel.log", "w+")
    kernel = subprocess.Popen([str(binary / "zend")], env=env, stdout=log, stderr=log)
    peers = []
    try:
        for _ in range(150):
            try:
                urllib.request.urlopen(url + "/health", timeout=1).close()
                break
            except Exception:
                if kernel.poll() is not None:
                    log.seek(0)
                    raise AssertionError(log.read())
                time.sleep(.1)
        else:
            raise AssertionError("kernel never became healthy")
        try:
            api("/api/shared-brain/sessions", {"agent":"zenbot"}, token="wrong")
            raise AssertionError("missing authentication")
        except urllib.error.HTTPError as e:
            assert e.code == 401

        def peer(agent, write=False):
            p = subprocess.Popen([str(binary / "zen"), "mcp", "serve", "--agent", agent] + (["--write"] if write else []),
                                 env={**env, "ZEN_URL":url}, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                 stderr=subprocess.PIPE, text=True)
            peers.append(p)
            return p
        seq = 0
        def request(p, method, params=None):
            global seq
            seq += 1
            p.stdin.write(json.dumps({"jsonrpc":"2.0","id":seq,"method":method,"params":params or {}}) + "\n")
            p.stdin.flush()
            line = p.stdout.readline()
            assert line, p.stderr.read()
            out = json.loads(line)
            assert out["id"] == seq
            return out
        def init(p):
            out = request(p, "initialize", {"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1"}})
            assert out["result"]["capabilities"] == {"tools":{}}
            p.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
            p.stdin.flush()
        def call(p, name, args):
            out = request(p,"tools/call",{"name":name,"arguments":args,"_meta":{"progressToken":"synthetic"}})
            assert "result" in out, out
            return out["result"]
        def text(r):
            return "\n".join(c.get("text","") for c in r["content"])
        z = peer("zenbot")
        assert request(z,"tools/list")["error"]["code"] == -32002
        init(z)
        names = {t["name"] for t in request(z,"tools/list")["result"]["tools"]}
        assert names == {"search","history","brain","wiki_read"}, names
        assert "shared knowledge" in text(call(z,"wiki_read",{"path":"page.md"}))
        for path in ["../outside.md", str(root / "outside.md"),"escape.md","escape-dir/outside.md"]:
            r = call(z,"wiki_read",{"path":path})
            assert r["isError"] and "outside secret sentinel" not in text(r), r
        for name, args in [("bash",{"command":"false"}),("read",{"path":"/etc/passwd"}),
                           ("call_tool",{}),("remember",{"text":"no"}),("brain",{"action":"put","record":{}})]:
            assert call(z,name,args)["isError"], name
        # Parse errors are recoverable; notifications produce no response.
        z.stdin.write("not json\n")
        z.stdin.flush()
        assert json.loads(z.stdout.readline())["error"]["code"] == -32700
        assert request(z,"unknown")["error"]["code"] == -32601
        assert request(z,"ping")["result"] == {}

        a = peer("alice", True)
        b = peer("bob", True)
        init(a); init(b)
        assert "remember" in {t["name"] for t in request(a,"tools/list")["result"]["tools"]}
        r = call(a,"remember",{"text":"Alice private memory unique purple sentinel","source":"verified"})
        assert not r["isError"], r
        assert not call(b,"remember",{"text":"Bob private memory unique orange sentinel","source":"verified"})["isError"]
        assert "alice private" in text(call(a,"wiki_read",{"path":"private.md"}))
        assert call(a,"wiki_read",{"path":"page.md","global":True})["isError"]
        assert call(a,"wiki_read",{"path":"../bob/wiki/private.md"})["isError"]
        # Agent identity cannot be overridden by tool arguments.
        r = call(a,"search",{"query":"orange sentinel","agent":"bob","scope":"memories"})
        assert "Bob private memory unique orange sentinel" not in text(r), r
        r = call(a,"search",{"query":"purple sentinel","scope":"memories"})
        assert "Alice private memory" in text(r), r
        r = call(a,"brain",{"action":"list","agent":"bob"})
        assert not r["isError"], r
        owner = api("/api/sessions",{"agent":"zenbot","model":"faux/smoke"})
        assert call(a,"history",{"session":owner["id"]})["isError"]
        # Normal sessions cannot access the narrow endpoint at all.
        try:
            api("/api/shared-brain/sessions/" + owner["id"] + "/tools")
            raise AssertionError("ordinary session accepted")
        except urllib.error.HTTPError as e:
            assert e.code == 403

        for p in peers:
            p.stdin.close()
            assert p.wait(timeout=10) == 0, p.stderr.read()
        db = sqlite3.connect(root / "zen.sqlite3")
        count, denied = db.execute("SELECT count(*),sum(is_error) FROM tool_calls t JOIN sessions s ON s.id=t.session_id WHERE s.kind IN ('mcp_read','mcp_write')").fetchone()
        assert count >= 19 and denied >= 10, (count, denied)
        assert db.execute("SELECT count(*) FROM sessions WHERE kind IN ('mcp_read','mcp_write')").fetchone()[0] == 3
        assert db.execute("SELECT count(*) FROM tape_events WHERE kind='mcp_call'").fetchone()[0] == count
        assert db.execute("SELECT count(*) FROM tool_calls WHERE result_sha256 IS NULL").fetchone()[0] == 0
        print(f"shared brain stdio e2e passed: {count} audited calls, {denied} refusals")
    finally:
        for p in peers:
            if p.poll() is None:
                p.kill()
            p.wait()
        kernel.terminate()
        try:
            kernel.wait(timeout=10)
        except subprocess.TimeoutExpired:
            kernel.kill(); kernel.wait()
        log.close()
