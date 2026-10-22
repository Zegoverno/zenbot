# zenbot — Progress Log

> Append-only. What shipped, what we learned, what changed and why. Newest entry at the top.
> Add an entry with every pull request that ships something; keep entries lean. Decisions go to
> DECISIONS.md, plans to ROADMAP.md.

---

## 2026-10-22 — Sign in to MCP servers from the terminal (D-072)

`zen mcp-login <server>` and `/mcp login <server>` run the MCP authorization flow: discovery,
dynamic client registration, PKCE; the owner opens the link anywhere and pastes back the address
the browser ended on. Tokens refresh by themselves; an expired sign-in says how to renew it. `/mcp`
lists servers with their sign-in state. An e2e scenario signs in against a mock OAuth server.

## 2026-10-21 — Phase 5: tools see images and PDFs, batch edits, bounded output, fuller MCP (D-071)

`read` shows images and selected PDF pages (text and a rendered page) to both engines; `edit`
applies up to 100 replacements atomically and returns a diff. Shell and MCP output past 12 KB is
previewed head and tail with the full text saved, bounded to 256 MB in all. MCP calls no longer wait
on unrelated servers; HTTP replies are read as they stream; structured results, images, resources,
prompts, refresh-token OAuth and owner-answered forms (`zen mcp-pending`, `zen mcp-respond`) work,
checked against a FastMCP reference server. Tool descriptions are shorter.

## 2026-10-21 — Hold duplicate legacy knowledge names

Migration holds duplicate titles/aliases before writing archives; a regression covers duplicate
titles. CLI help and development instructions describe migration and retired tools.

## 2026-10-21 — Finish Phase 4's legacy paths (D-070)

Owner-run preview/apply migrates flat wiki pages into provisional source records, retaining all
prose, exact private originals, writer ratings and resolvable links. Conflicts and oversized pages
stay held; retries preserve owner edits. `capture` and made-tool authoring/execution are retired;
old tool files remain inspectable but approvals cannot reactivate them. The nightly `wiki` job now
indexes and lints records, reporting counts. Tests cover migration, retries, provenance after denial
and stale tool calls; real-work retrieval validation remains a release check.

## 2026-10-21 — Phase 4: memory, knowledge and skills in Markdown; search follows links (D-069)

Short-term memory is one capped `MEMORY.md` in the instructions: `remember` never refuses (the sleep
runs when it's full), System One keeps lasting sentences of the owner's words, and the owner's edits
are read back. Long-term memory, knowledge (with how we know it) and people/organizations/projects are
Markdown records with tags, aliases and typed links; the database only indexes them. The sleep
promotes lasting memories and archives finished ones, never touching prompt files; `long: true`
needs a higher bar. Nothing is injected per turn: `search` (new `work/search` skill) follows links
System One keeps, shows contradictions and replaces superseded records. Reworked after the PR #86
review: per-turn recall, database-authoritative memory, undo, work queues, synthesis and shadow mode
removed; replacing a memory keeps every writer. Tests: unit, e2e `memory`, `system-one`,
`taint-on-data`, `phase4`, `phase4-upgrade` (a migration-34 database shaped like the live one).

## 2026-10-19 — Phase 2b and Phase 3: approved jobs and kernel recovery (D-068)

Owner-approved jobs retain their named sources and workspace; agent edits reset that authority.
Source judgments distinguish identifiable publishers from unknown ones, and aligned file edits
can proceed at high risk. Held jobs wait for owner queue decisions and then finish their reports.
MCP stdio servers use containment and their own egress policy; status no longer connects them.
`zen trust suggest` proposes entries for review. Busy prompts queue durably in the kernel;
`/steer` and Matrix `!steer` interrupt at the next tool boundary. Worker writes are bounded,
tool bookkeeping errors return a result, summary flags clear on task exit, and nested permission
checks share an invalidated snapshot. JSONL session mirrors retry from a durable export queue.
Legacy workflow blocks no longer hide history. Validation is recorded in the PR before merge.

## 2026-10-16 — Security: context risk, action alignment, contained sessions (D-067)

Every shell now runs contained (no secrets, no sudo, no host sockets) with its network through a
per-session egress proxy in the kernel; acting as the owner is a `host: true` command the gate
checks. One S1 decision (`action_gate`) runs, holds or locks each effect from the session's context
risk, the action's effect and, only when needed, alignment with the owner's job; most actions need
no model call. The owner's trust list (`zen trust`, `~/.zenbot/trust.toml`) covers URLs, APIs, MCP
servers and repos by remote. Rating fixes: legacy marks reset (migration 0033), clones rated by
their remote, engine errors and medium products no longer raise risk. Removed: the shell grammar,
the SafeRead/offline modes as policy, the task class table. Tests: unit (classify, rules on inputs,
contained-shell enforcement, proxy parsing, trust parsing), e2e `security` (proxy end to end, holds,
owner allow, no sudo/Docker/token), the gate scenarios rewritten; replay: 7,263 of 7,284 low-risk
actions run without a model call, none held by rule.
