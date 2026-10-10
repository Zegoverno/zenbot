# Skills, deferred tools and MCP: research for zenbot's redesign

> Dated research snapshot (2026-10-06), kept as history. The Pi worker it mentions is retired, skills
> now live in `~/.zenbot/global/skills/` (D-040), and the clones and `../spike/` files were not kept.
> What zenbot does today is in [DESIGN.md](../../DESIGN.md) and [MAP.md](../../MAP.md).

Date 2026-10-06. Sources cloned with `git clone --depth 1` into this directory:
agentskills/agentskills `69ef37e` (2026-08-09), anthropics/skills `683bc88` (2026-10-05),
jlowin/fastmcp `5baeacf` (2026-10-04), modelcontextprotocol/rust-sdk `08e0211` (2026-10-06),
MineDojo/Voyager `55e45a8` (2023-07-27). Spike scripts and outputs: `../spike/`.
Context read: DESIGN.md "Target design", ROADMAP.md Phases 1-2, `crates/zen-engine/src/{claude,bridge,codex}.rs`.

## 1. Agent Skills format (agentskills.io, anthropics/skills)

Spec: `agentskills/docs/specification.mdx` (= https://agentskills.io/specification; `skills/spec/agent-skills-spec.md`
now only links there). Reference validator: `agentskills/skills-ref/src/skills_ref/validator.py`.
Anthropic's stricter one: `skills/skills/skill-creator/scripts/quick_validate.py`.

- **Layout:** `skill-name/SKILL.md` required; optional `scripts/` (executable), `references/` (docs read on demand),
  `assets/` (templates, data). Any other files allowed.
- **Frontmatter** (YAML between `---` lines). Only these keys; both validators reject any other key:

| Field | Req. | Constraint |
|---|---|---|
| `name` | yes | 1-64 chars; lowercase `a-z0-9` and `-`; no leading/trailing `-`; no `--`; **must equal the parent directory name** (NFKC-normalised) |
| `description` | yes | 1-1024 chars, non-empty; what it does **and when to use it**, with trigger keywords. quick_validate also forbids `<` `>` |
| `license` | no | short name or bundled file |
| `compatibility` | no | 1-500 chars; environment needs (packages, network) |
| `metadata` | no | map string→string, for client-specific keys (use unique key names) |
| `allowed-tools` | no | space-separated pre-approved tools, experimental |

- **Body:** free Markdown. Recommended: SKILL.md < 500 lines / < 5000 tokens; move detail to `references/`;
  file references relative to the skill root and **one level deep** (no chains).
- **Progressive disclosure** (`docs/client-implementation/adding-skills-support.mdx`): tier 1 catalog
  (name+description, ~50-100 tokens/skill) at session start; tier 2 full body on activation; tier 3 resources
  only when the body points to them. Client guidance worth copying: a dedicated activation tool with the `name`
  constrained to valid names; return the body wrapped as `<skill_content name="…">` plus the skill dir and a
  **listing (not contents)** of bundled files; dedupe activations per session; **protect skill content from
  compaction**; hide disabled skills from the catalog entirely; project-level skills are untrusted until the
  project is trusted; lenient on load (warn on bad name, skip on missing description/unparseable YAML).
- **Description is the trigger.** skill-creator (`skills/skill-creator/SKILL.md` l.67, l.398): models
  under-trigger, so descriptions should be a bit "pushy"; models skip skills for tasks they can do in one step.
  It optimises descriptions with a train/held-out trigger eval (`run_loop.py`), selecting by held-out score.

## 2. Deferred tool loading and the prompt cache

**Anthropic API tool search** (https://platform.claude.com/docs/en/agents-and-tools/tool-use/tool-search-tool):
every tool is still sent on every request; `defer_loading: true` keeps it out of the context. "Internally, the API
excludes deferred tools from the system-prompt prefix. When Claude discovers a deferred tool …, the API appends a
`tool_reference` block inline in the conversation, then expands it into the full tool definition … The prefix is
untouched, so prompt caching is preserved." Server variants `tool_search_tool_regex_20251119` /
`tool_search_tool_bm25_20251119` (5 results default, ≤10,000 deferred tools). **Custom search** is allowed: any
client tool may return `{"type":"tool_result","content":[{"type":"tool_reference","tool_name":"x"}]}` if `x` is in
`tools` with `defer_loading`. At least one tool must be non-deferred; deferred tools can't carry `cache_control`.
Advice: keep the 3-5 most used tools non-deferred; namespace names (`github_…`); say in the system prompt which
tool categories exist. Selection accuracy degrades past 30-50 loaded tools.

**Claude Code** (https://code.claude.com/docs/en/mcp, "Scale with MCP tool search"): MCP tools are deferred by
default (`ENABLE_TOOL_SEARCH` unset/`true`/`auto[:N]`/`false`); only names + server instructions load at start;
the model loads them with the built-in `ToolSearch` (returns `tool_reference`s). A server or a single tool
(`_meta: {"anthropic/alwaysLoad": true}`) can opt out of deferral. Descriptions truncated at 2,048 chars.
On `list_changed`: "In non-interactive mode with the `-p` flag …, Claude Code refreshes only the tool list."

**Spike (Claude Code 2.1.289, Haiku 4.5, `../spike/srv*.py`, outputs `out*.jsonl`):** a stdio MCP server whose
`unlock` tool adds `secret_word` and sends `notifications/tools/list_changed`.

| Run | Flags | Result | Cache (per request: created / read) |
|---|---|---|---|
| 1 | zenbot's flags (`--tools ""`) | init tools `[mcp__t__unlock]` (no ToolSearch → loaded upfront). List refresh is async: the first `secret_word` call fails "No such tool", the next succeeds | 7339/0 · 170/7339 · **7981/0** · 193/7981 |
| 2 | `--tools ToolSearch`, `ENABLE_TOOL_SEARCH=true`, `unlock` alwaysLoad | same failure, then model runs `ToolSearch select:…` and calls it | 1024/6320 · 163/7344 · **8247/0** · 238/8247 · 124/8485 |
| 3 | as 2, but `secret_word` listed (deferred) from the start, no list change | `ToolSearch` → `tool_reference` → call works first time | 1085/6725 · 180/7810 · 217/7990 · 116/8207 (**no miss**) |

So: (a) `claude -p` **does** honour `tools/list_changed` mid-turn over stdio, but (b) any change to the tool list
**rewrites the whole cached prefix** (with or without tool search), and (c) the refresh races the next request.
(d) Deferral is cache-safe only when the deferred tool is in the list from the start. Since each zenbot turn is a
new `claude` process resuming the session, the bridge's `tools/list` must also be byte-identical across turns.
Third-party issue mirrors (claudeissues.com #24195, #50515, #78208) report earlier versions ignoring `list_changed`
or regressing on HTTP; don't depend on it.

**Codex** gets zenbot's tools as app-server `dynamicTools` at thread start (`codex.rs` l.162-206): fixed per
thread; no evidence of list-change support (not verified). **Pi** builds its own request each turn
(`packages/mind/src/main.ts` l.155); for OpenAI-style APIs tools also sit at the head of the prompt, so changing
them breaks the cache there too.

## 3. FastMCP (Python): ideas worth copying

Docs under `fastmcp/docs/`, source `fastmcp/fastmcp_slim/fastmcp/`.
- **Client** (`docs/clients/client.mdx`): one `Client(source)` infers transport (in-memory, stdio, HTTP);
  `list_tools` / `call_tool`; a **config dict `{"mcpServers": {...}}` makes one aggregate client whose tools are
  prefixed by server name** (`weather_get_forecast`); listing cache with TTL.
- **Composition** (`docs/servers/composition.mdx`, `server.py` `mount()` l.2285): `main.mount(child, namespace="x")`,
  live (child changes show through); `create_proxy(url|path|config)` mounts external servers. Namespace transform
  (`transforms/namespace.py`): tools `x_name`, resources `data://x/…`.
- **Tool transformation** (`docs/servers/transforms/tool-transformation.mdx`, `transforms/tool_transform.py`):
  rename, re-describe, tag, annotate, enable/disable; per-argument rename, describe, default, **`hide` with a
  fixed default** (inject secrets/user ids the model never sees), make required, change type; `transform_fn` wraps
  execution and calls `forward()`. The same transforms can be declared **in the config file** per server
  (`docs/clients/transports.mdx` l.240-280: `"tools": {"orig": {"name":…, "arguments": {"city": {"default":…,
  "hide": true}}}}`, plus `include_tags`/`exclude_tags`).
- **Middleware** (`docs/servers/middleware.mdx`, `server/middleware/`): onion pipeline with hooks from general to
  specific (`on_message` → `on_request` → `on_call_tool`/`on_list_tools`); `call_next` or short-circuit. Built-ins:
  error handling, logging, timing, rate limiting, caching, **response_limiting** (cap output size),
  authorization, tool_injection.
- **Tool search transform** (`docs/servers/transforms/tool-search.mdx`): replaces the listing with two synthetic
  tools, `search_tools` (regex or BM25 over names, descriptions, arg names/descriptions; returns full schemas) and
  `call_tool(name, args)`; hidden tools remain callable. This is exactly zenbot's fallback design, shipped.
- **Code mode** (`transforms/code-mode.mdx`): `search` → `get_schema` → `execute` script calling `call_tool`
  in a sandbox (Cloudflare/Anthropic "code execution with MCP").
- **Visibility** (enable/disable by name/tag at runtime), **tool fingerprinting** (sha256 of canonical
  `{key, inputSchema}` to detect schema drift), **SkillsDirectoryProvider** (skills as `skill://name/SKILL.md`
  resources plus a `_manifest` with sizes and sha256).

## 4. rmcp, the official Rust MCP SDK

`rust-sdk/crates/rmcp`, **version 3.5.1** (crates.io, 2026-10-05; 3.x releases roughly weekly), MSRV 1.88.
Client features: `client`, `transport-child-process` (stdio subprocess via `process-wrap`),
`transport-streamable-http-client-reqwest` + one TLS choice (`reqwest` = rustls with its default provider,
`reqwest-tls-no-provider`, `reqwest-native-tls`); `auth` adds OAuth2. `default-features = false` drops the
server/macros. Examples: `examples/clients/src/{git_stdio,streamable_http,collection}.rs`.
Notifications: implement `ClientHandler::on_tool_list_changed` (`src/handler/client.rs` l.241).
`list_all_tools()` paginates (`src/service/client.rs` l.1760). HTTP config:
`StreamableHttpClientTransportConfig::with_uri(..).auth_header(..)`, `custom_headers`.

**Weight** (`cargo tree`, `../rmcptree/`): the client features above pull **25 crates zend doesn't have**:
reqwest **0.13** (the `zen` CLI already uses reqwest 0.12 → two versions), rustls with **aws-lc-rs/aws-lc-sys**
(C/cmake build), hyper-rustls, tower-http, process-wrap, nix, sse-stream, futures, tokio-util… Mitigate with
`reqwest-tls-no-provider` + installing ring (already in Cargo.lock via `zen`), and moving `zen` to reqwest 0.13.

```rust
// zend Cargo.toml:
// rmcp = { version = "3.5", default-features = false, features = ["client", "transport-child-process",
//          "transport-streamable-http-client-reqwest", "reqwest-tls-no-provider"] }
use rmcp::{model::CallToolRequestParams, service::{RunningService, ServiceExt}, RoleClient};
use rmcp::transport::{ConfigureCommandExt, StreamableHttpClientTransport, TokioChildProcess,
                      streamable_http_client::StreamableHttpClientTransportConfig};

async fn connect_stdio(cmd: &str, args: &[String], env: &[(String, String)]) -> anyhow::Result<RunningService<RoleClient, ()>> {
    let t = TokioChildProcess::new(tokio::process::Command::new(cmd).configure(|c| {
        c.args(args).env_clear().env("PATH", std::env::var("PATH").unwrap_or_default()).envs(env.iter().cloned());
    }))?;
    Ok(().serve(t).await?) // () = ClientHandler with no-op callbacks; use a struct to catch list_changed
}
async fn connect_http(url: &str, bearer: Option<String>) -> anyhow::Result<RunningService<RoleClient, ()>> {
    let mut cfg = StreamableHttpClientTransportConfig::with_uri(url);
    if let Some(b) = bearer { cfg = cfg.auth_header(b); }
    Ok(().serve(StreamableHttpClientTransport::from_config(cfg)).await?)
}
async fn demo(c: &RunningService<RoleClient, ()>) -> anyhow::Result<()> {
    let tools = c.list_all_tools().await?;                         // Vec<Tool>: name, description, input_schema
    let args = serde_json::json!({ "repo_path": "." }).as_object().cloned().unwrap();
    let r = c.call_tool(CallToolRequestParams::new("git_status").with_arguments(args)).await?;
    // r.content: Vec<Content> (text/image/resource), r.is_error: Option<bool>, r.structured_content
    c.cancel().await?; Ok(())
}
```
(API names checked against 3.5.1 source: `from_config` is in `transport/common/reqwest/streamable_http_client.rs`
l.388; the rest follows the examples. Not compiled.)

## 5. Voyager's skill library

`Voyager/voyager/agents/skill.py`, `voyager/voyager.py`, prompt `voyager/prompts/skill.txt`.
- **Added** only after the critic agent judges the task a success (`voyager.py` ~l.350 `add_new_skill(info)`):
  a skill is executable JS code; skills that are pure busywork are skipped by a hardcoded filter.
- **Description is generated, not authored:** an LLM summarises the code (≤6 sentences, one line, no function
  name); that text is what gets embedded (OpenAI embeddings in Chroma), keyed by the program name.
- **Dedup is by name only:** an existing name is overwritten in the index and saved as `nameV2.js` on disk. No
  semantic dedupe, no pruning, no usage counts; the library only grows.
- **Retrieved** top-k=5 by similarity of the task *context* (curriculum's question-answer context), and again
  after each failed attempt with the context plus a summary of the chat log (`voyager.py` l.184, l.245).
- **Composed** by code: retrieved skills' source is put in the action agent's system message and new programs
  call them as functions; all programs plus control primitives are loaded into the runtime.
Lessons: verify before adding; describe from what the skill *does*; retrieve against the current need, and
re-retrieve on failure; the weak points (no dedupe, no pruning) are what zenbot's anti-sprawl rules must add.

## Recommendation for zenbot

**SKILL.md format.** Adopt the agentskills spec unchanged, so any skill from anthropics/skills or other clients
loads, and zenbot's skills work elsewhere. Path `~/.zenbot/skills/<domain>/<name>/SKILL.md` (domain = directory,
same name rules; the spec only constrains the skill's own directory). zenbot-only data goes in `metadata` with
`zen-` keys, e.g. `zen-status: draft|active|archived`, `zen-origin: owner|agent|import:<url>`. No new top-level
keys (validators elsewhere reject them).

**Validation on save.** The kernel validates whenever `write`/`edit` touches `skills/**`, before the file lands
(write to temp, validate, rename), and the tool result lists every error. Strict for zenbot's own skills:
1. Frontmatter parses; keys ⊆ {name, description, license, compatibility, metadata, allowed-tools}.
2. `name` rules above and equals its directory; unique across all domains.
3. `description` 1-1024 chars, no `<`/`>`, states when to use (warn if no "use when"-style clause).
4. `compatibility` ≤ 500; `metadata` values are strings.
5. Body ≤ 500 lines and ≤ ~5k tokens (hard cap, error); reference files linked from SKILL.md exist, are inside the
   skill dir and one level deep; each reference file ≤ a fixed size.
6. Secret scan with the same masker as tool output; reject on hit.
7. Near-duplicate check (anti-sprawl below).
Imported or repo skills: lenient on load (agentskills guidance), diagnostics in `zen status`. Repo-level skills
load only from trusted projects.

**find_skills / load_skill.** Prefix holds only the domain index (domain, one line, skill count), never per-skill
lines. `find_skills(need)` → top 5 `{name, domain, description}`: lexical (BM25/substring over name +
description) first, System One rerank when configured, later pgvector (Phase 3). `load_skill(name, file?)` →
body (frontmatter stripped) as `<skill_content name dir>` plus a listing of `references/ scripts/ assets/`; with
`file`, one reference file. `name` validated against the catalog (the schema can't carry an enum without
changing the prefix). Appended as a tool result, so it is cache-safe on every engine. Per session: dedupe repeat
loads ("already loaded at turn N"), never compact skill results, record each load (session, turn, skill, file,
bytes) and later whether it was used.

**find_tools / load_tool.** Catalog = MCP servers' tools, namespaced `server__tool`, plus agent-made tools.
Rules from the spike: **never change an engine's tool list mid-session** (full cache rewrite, plus a racy failed
first call), and **freeze the catalog snapshot per session** (fingerprint hash of name + schema + description,
FastMCP-style) so every resumed turn sends identical tools.
- *Baseline, all engines (cache-safe, uniform):* the fixed prefix holds the system tools plus one generic
  `call_tool(name, args)`. `find_tools(need)` returns names + one-liners; `load_tool(name)` returns the full
  definition (description + JSON schema) as a tool result; the model then calls `call_tool`. The kernel validates
  `args` against the schema and returns precise errors (losing the API's schema-constrained decoding is the cost).
  This is FastMCP's search transform, and works for Claude Code, Codex `dynamicTools` and Pi alike.
- *Claude-only option, decide by eval:* the bridge lists the session's whole catalog snapshot with system tools
  marked `_meta.anthropic/alwaysLoad`, and runs `claude` with `--tools ToolSearch` (and
  `ENABLE_TOOL_SEARCH=true`); the model loads tools with Claude Code's `ToolSearch` (spike run 3: cache intact,
  first call works). This uses an engine-native tool, against "engine-native tools are not used", so it needs the
  owner's OK; measure its gain in call accuracy against the baseline. For direct Anthropic API engines, zenbot's
  own `find_tools` can return `tool_reference` blocks with the catalog sent as `defer_loading` (official custom
  tool search path), cache-safe.
- Agent-made tools enter the catalog only from the next session (the snapshot is frozen).

**MCP client.** In the kernel (it executes every tool): `rmcp` 3.5 with `default-features = false` and
`client, transport-child-process, transport-streamable-http-client-reqwest, reqwest-tls-no-provider` (+ ring
provider; align `zen` on reqwest 0.13). Connect lazily on first `find_tools`/call and cache `tools/list` per
server; honour `on_tool_list_changed` by refreshing the catalog for *new* sessions only. Every MCP call passes the
same kernel middleware chain as system tools, in order: audit (`tool_calls` row, server, latency) → permission
(allow/ask/deny per tool) → secret injection (hidden args/headers) → timeout → output size cap and secret masking
→ taint mark (MCP output untrusted, SPEC §5.18). Stdio servers run in the sandbox with only declared env.

`~/.zenbot/mcp.json` — Claude Code/FastMCP-compatible `mcpServers` shape plus zenbot keys:
```json
{ "mcpServers": {
    "github": { "type": "http", "url": "https://api.githubcopilot.com/mcp/",
      "headers": { "Authorization": "Bearer ${secret:GITHUB_TOKEN}" },
      "namespace": "gh", "enabled": true, "timeout_s": 60, "max_output_chars": 50000,
      "tools": { "include": ["get_issue", "create_pull_request"], "exclude": [],
        "transform": { "create_pull_request": { "description": "…", "permission": "ask",
                        "arguments": { "owner": { "default": "Zegoverno", "hide": true } } } } } },
    "sqlite": { "command": "uvx", "args": ["mcp-server-sqlite", "--db", "${HOME}/data.db"],
      "env": { "LOG_LEVEL": "warn" }, "namespace": "db", "permission": "allow" } } }
```
`${VAR}` as in Claude Code; `${secret:NAME}` resolved from the kernel's secret store, never shown to the model.
Validated on save like skills (unknown keys, bad URLs, missing secrets → error).

**Anti-sprawl rules.**
1. A new skill must not duplicate one: on save, `find_skills(description)`; System One judges "same kind of work
   as X?" and on a match the save fails with "extend X instead" (Voyager only dedupes by name).
2. Agent-written skills and tools start `zen-status: draft`, are committed to git, and become `active` only when
   the owner accepts them or after a verified successful use (Voyager adds only verified skills).
3. Descriptions of agent-made tools are generated from what the code does (Voyager), then reviewed.
4. Measure per load: loaded, used after loading, session outcome. Sleep flags skills/tools not loaded in 60 days
   or loaded-but-unused in most loads → archive proposal; a skill loaded in most sessions of a kind is a candidate
   for that kind's prefix (DESIGN.md).
5. Hard caps: SKILL.md 500 lines; description 1024 chars (aim ≤ 300); refs one level deep; per MCP server an
   explicit `include` list once it has more than ~20 tools; tool descriptions truncated at 2048 chars.
6. One concern per skill, one name space: `domain/name`, MCP tools `ns__tool`; collisions rejected at save.
