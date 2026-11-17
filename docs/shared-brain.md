# Shared brain for external harnesses

Run `zen mcp serve --agent zenbot` as a local newline-delimited JSON-RPC stdio MCP
server. Choose an agent explicitly: this is a scope choice, not a label. Other agents
see their own sessions and vault, and shared knowledge only as their agent settings
permit. Use a dedicated restricted agent when a harness should not see everything.

The bridge connects to the running kernel using the usual `ZEN_URL` and local
`$ZEN_HOME/token` (or `ZEN_TOKEN`). Credentials go only in the HTTP Authorization
header, never in MCP results or configuration checked into source control.
Do not expose the kernel or bridge to untrusted users: the kernel owner token is
an owner credential, not a per-agent credential. Configure local trusted harnesses only.

Example Claude-compatible MCP configuration (use the absolute path to your installed zen):

```json
{
  "mcpServers": {
    "zen-brain": {
      "command": "/home/you/.zenbot/bin/zen",
      "args": ["mcp", "serve", "--agent", "zenbot"]
    }
  }
}
```

Codex `~/.codex/config.toml`:

```toml
[mcp_servers.zen-brain]
command = "/home/you/.zenbot/bin/zen"
args = ["mcp", "serve", "--agent", "zenbot"]
```

Default tools: search, history, brain get/list/schema, wiki_read. The kernel also applies
the agent's tool allowlist. Add `--write` to opt in to remember and brain put.
There is no shell, general file access, skill mutation, downstream MCP passthrough,
or unrestricted search/brain API proxy.

Each launch creates a dedicated SQLite session, marked mcp_read or mcp_write. The
kernel derives the agent and mode from that session for every request. Tool arguments
cannot override either. Calls (including policy refusals) go through the kernel's
tool runner, secret masking, output limits, SQLite tool_calls audit and session tape.
The new authenticated endpoints are POST /api/shared-brain/sessions and
GET/POST /api/shared-brain/sessions/{id}/tools; ordinary sessions cannot use them.

wiki_read takes a relative Markdown path under the configured wiki root
(ZEN_WIKI_DIR for zenbot; the agent's own vault/wiki otherwise). Set global=true
for the shared wiki if the agent is permitted to read it. Absolute paths, traversal,
descendant symlinks and special files are refused. Files are opened with directory
FDs and O_NOFOLLOW, so symlink swaps cannot escape the root. The configured root is
required to be a real directory, not a symlink. Pages are capped at 200000 bytes.

Protocol: initialize, notifications/initialized, tools/list, tools/call and ping;
one JSON object per line, at most 1 MiB per frame. Stdout contains only protocol
responses; errors go to stderr. Existing zen mcp list/login/etc. remain unchanged.
