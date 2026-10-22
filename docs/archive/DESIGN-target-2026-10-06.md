# Historical target design

Preserved from main d560bfc before Phase 8; current behavior is in DESIGN.md.

## Target design (agreed 2026-10-06)

Not built yet unless marked; most of it has shipped since (see "As built"). The roadmap builds it
in phases. Sources: the Phase 0 research in
`docs/archive/research/` (Hermes, OpenClaw, agentskills.io, Anthropic's tool search, FastMCP, rmcp, Voyager,
Letta, LLM Wiki and gbrain, Postgres hybrid search, web providers and fetch safety), each claim with
file paths in the projects' code.

### Principle

Give the agent tools, skills and context; don't push it through a workflow (D-026). Fixed rules only
for authority (the owner starts jobs and sets the budget; the agent never decides the owner's calls),
safety (every action through the kernel, sandboxed, secrets masked, audited) and measurement (the
tape). A rule comes back only where measurement shows the agent needs it.

### Engine

- **Provider-free.** Claude Code and Codex on subscriptions, System One classifiers such as
  Jev through OpenRouter; always the latest. zenbot owns skills, tools and context, so any model can do any session or
  subtask. Engine-native skills and tools are not used.
- **Context management stays as built.** A model switch re-sends the whole context, so switches
  happen at natural boundaries (a new session or subtask).
- **Only what the job needs enters the context.** Everything loaded after session start is appended
  as a tool result; the prefix never changes mid-session. Every load is recorded per turn, and what is
  loaded but never used is measured and steered away. A skill or tool loaded in most sessions of a
  kind can move into that kind's prefix, on data.

### What the agent gets, and when

| When | What |
|---|---|
| Session start (fixed prefix) | `SOUL.md` · `IDENTITY.md` · `AGENTS.md` · `USER.md` · `MEMORY.md` (fixed size) · the system tools with their descriptions · a short index of skill domains · a repo's own `AGENTS.md` as project context |
| On demand (appended) | skills, connected MCP servers' tools, tools the agent made |

| File in `~/.zenbot/` | Says | Owner |
|---|---|---|
| `SOUL.md` | the deep layer: what the agent is for, what it holds to, lines it doesn't cross | owner only; agent may suggest |
| `IDENTITY.md` | the agent in practice: character, how it works, learned guidance (D-045) | agent writes what really matters, kept compact; the owner may edit |
| `AGENTS.md` | its environment: the VM, its body, where things live, what it can reach | owner; agent proposes |
| `USER.md` | the owner: who they are, their preferences, their context | owner; agent writes what really matters, kept compact |
| `MEMORY.md` | where things stand (not traits or preferences) | agent, within a fixed size |

**Who teaches what:** `AGENTS.md` the environment; each tool's own description how and when to use
that tool (what it does, when to use it and when not, what it returns, an example); skills how to do
a kind of work well. Tool descriptions are paid on every turn, so they grow only where the model is
measured misusing a tool.

### System tools

Loaded at session start.

| Tool | What it does | Today |
|---|---|---|
| `bash` | Run commands in the sandbox | exists |
| `read` | Read a file or image | exists |
| `write` | Create or overwrite a file | exists |
| `edit` | Exact string replacement in a file | exists |
| `ask` | Bring the owner 1–3 questions, each with 2–4 options, recommended first; unanswered → the recommendation, recorded as an assumption. `wait: false` keeps working on what doesn't depend on the answer | built (D-038 added `wait: false`) |
| `search` | One search across sessions (this one included), memories and the wiki | built (D-035, D-036) |
| `remember` | Add, replace or remove a short-term memory entry, with its source | built |
| `web_search` | Search the web through the configured provider | built |
| `web_fetch` | Fetch a URL as readable text, with its links | built |
| `find_skills` | Search skills by need: names and one-line descriptions | built (word match; System One ranking later) |
| `load_skill` | Load a skill, or one of its reference files | built |
| `save_skill` | Create or improve a skill, from evidence | built (D-037) |
| `find_tools` | Search MCP tools by need: names and one-line descriptions | built (word match) |
| `load_tool` | Load a tool's full definition so it can be called | built |
| `call_tool` | Run a loaded tool (the list stays fixed, D-033) | built |
| `judge` | Ask System One one question over many items, with a probability each | built (D-066) |
| `verify` | A fresh verifier checks work against criteria, without the maker's reasoning | built |
| `delegate` | Hand subtasks to subagents with fresh context and a chosen model, in parallel | built (D-038) |
| `schedule` | Create, change or remove a scheduled job | built (D-046) |

Gone (2026-10-07): `move` (`bash mv`), `propose_brief` (a brief is a file the `brief` skill
writes), `submit_work`, `note_ruling` and the approvals. Going: `history`, once `search` covers it
(`search` shipped in D-035; `history` is still offered, and reads messages by number). Skills are authored through `save_skill` and activated by owner fingerprint review; curated
records through `brain` (D-069). `capture` and made-tool execution are retired (D-070).

### System One, used heavily

- **Called by the brain** through `judge`: one question over up to 200 items, an answer and a
  probability each, instead of reading and judging many things itself (D-066).
- **Built into tools** as decision files, so every engine benefits: `search` and `web_search` rank
  by relevance, `web_fetch` keeps the parts that help, the sleep judges memories,
  sources and content are rated, risky actions are checked against the job. `find_skills` and
  `find_tools` still match words.
- **Recorded and learned from:** every decision with its probabilities and, when known, what really
  happened; calibration applies where a file opts in, and changes reach a file only through the
  owner's approval.
- System One decides (where, whether, which); generative work (writing text) goes to a model.
- **Private content:** System One may see it unless the owner sets `ZEN_S1_PRIVATE=0` (D-032).

### Memory and knowledge

| Kind | In zenbot | Where |
|---|---|---|
| Working | `MEMORY.md`, fixed size | `global/MEMORY.md` with its entries' ratings in `memories`, rendered into the prompt |
| Experience (what mattered) | long-term memory | one Markdown record per memory in `global/memory/` |
| Episodic (what happened) | sessions, the tape | Postgres, searchable; an archive, not memory |
| Semantic (what's true, how we know) | knowledge records, the wiki | Markdown with evidence and typed links |
| Identity (who acts, for whom) | `SOUL.md`, `IDENTITY.md`, `USER.md`; people and organizations | prompt files (D-045); entity records |
| Procedural (how to do things) | skills | markdown and scripts in git |
| External | `web_search`, `web_fetch` | web content is untrusted input (taint rule, SPEC.md §5.18) |

**Short-term memory.** Saved with `remember`, or picked from the owner's own words after a turn;
each entry keeps its text, kind, source and every session that wrote it. Fixed size; past it the
sleep runs at once.

**Sleep.** System One asks of every short-term memory whether it lasts (it moves to long-term
memory as a record citing it) and whether it is finished (archived, never deleted); costs in the
decision files set the bars. Long-term memory made on the spot is rare (a higher bar).

**Records.** Knowledge, people/organizations/projects and long-term memory are files; their
metadata says how we know (evidence and its basis) and how they relate (typed links). Subjects and
tags classify across folders; aliases name the same record.

**Search.** One index over sessions, memories, the wiki and records: exact names and paths first,
full text (also English and Portuguese stemming), then pgvector, merged by rank, reranked by System
One, which also guides how far search follows records' links. A tool the brain uses, taught by the
`work/search` skill; nothing is injected every turn. Every search logged.

**Web.** `web_search` behind one provider contract (swappable providers); `web_fetch` with
readable-text extraction and link following; a browser later.

### Skills and tools that improve themselves

**Skills** use the agentskills.io format (`SKILL.md` with frontmatter, `references/`, `scripts/`) in
`~/.zenbot/global/skills/<domain>/<skill>/`, in git. Rules against sprawl (D-029): domains first (a new
domain needs the owner's OK); edit before create (search first; a new skill only when none covers
the work, with the reason recorded); changes from evidence at session close, never from one task;
small and composable; loads and outcomes measured, near-duplicates merged, unused skills retired;
every change a revertible commit. `save_skill` and the nightly `skills` job enforce these (D-037).

**MCP client** written against the spec (D-033; built). From FastMCP: namespacing and mounting
(`<server>_<tool>`, built), and not built yet: tool transformation (rename, hide arguments, rewrite descriptions), middleware
(audit, permissions, secret injection), proxying (zenbot's own tools as an MCP server, SPEC.md §3).

**Agent-made tools** are retired (D-070). Existing artifacts remain on disk for inspection; the
kernel does not publish or execute them. Procedures belong in reviewed skills with scripts.

**Loading tools mid-session** would change the tool list, which rewrites the cached prefix on every
engine (the Phase 0 spike), so the list stays fixed and `call_tool` runs whatever `load_tool`
showed (built).

### Model choice

Learned from real usage, not fixed tasks (D-030, Phase 6): a versioned policy maps the kind of work
to a model (subtasks take the `unknown` route since System One stopped classifying them, D-066); a small share of subtasks explore another model with the choice
probability logged; outcomes (verdicts, corrections, verification, cost) update the policy. Built
for subagents (D-038, "Delegation" above: the owner's verdicts and cost are the outcomes so far); a
main session runs on the model the owner picks.
