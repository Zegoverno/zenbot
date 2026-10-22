# zenbot — Progress Log

> Append-only. What shipped, what we learned, what changed and why. Newest entry at the top.
> Add an entry with every pull request that ships something; keep entries lean. Decisions go to
> DECISIONS.md, plans to ROADMAP.md.

---

## 2026-10-22 — Preserve all-word search and exercise SQLite fixtures

Full-text search requires every literal query word, preserving PostgreSQL’s ordinary multiword
behavior so typed-link expansion still brings in related records. A partial-word regression guards
this behavior. End-to-end fixtures use SQLite UUID blobs, booleans and failure-injection triggers.

## 2026-10-22 — Account for the bundled SQLite binary

The kernel release size budget includes SQLite, FTS5 and sqlite-vec: measured at 22,974 KiB
locally and 22,965 KiB in CI, with about 15% headroom. Client and worker budgets stay fixed.

## 2026-10-22 — Retain the PostgreSQL project during staged cutover

Staged upgrades select the original Compose project when exporting PostgreSQL, so moving the
Compose file beside tested binaries still reaches the retained source. Cutover regression coverage
checks the exact exporter command; storage and CI instructions match the SQLite implementation.

## 2026-10-22 — Phase 8: SQLite state and portable archives (D-076)

The kernel uses a private SQLite database with bundled sqlite-vec and FTS5, without a Docker
runtime requirement. Existing PostgreSQL state migrates through a versioned JSONL snapshot:
IDs, vectors, provenance and historical tape hashes survive; validation precedes publication.
Consistent SQLite backups include WAL. Search can use an external SearXNG or Brave/Tavily.

## 2026-10-22 — Phase 7: engines (D-075)

Codex counts tokens once with an estimated cost, so the router compares engines fairly. At most 3
background engines run at once (the owner's never wait). Read-only tool calls run in parallel;
writes and the shell one at a time. Codex drops its bundled skills, engine state is pruned after
14 days, and a rolled-back CLI update fails the `engines` job with its reason.

## 2026-10-22 — Phase 6: MCP sign-in, script jobs, the learning router (D-072 to D-074)

`zen mcp-login` / `/mcp login` sign in to remote MCP servers by pasting back the browser's address.
A report that can't be delivered no longer stops the schedule. Script jobs run a command with no
model; one zenbot writes waits for the owner's approval, and each run logs its command and exit
code. The learning router picks each unpinned session's, job run's and subtask's model by its kind
of task (System One), tries candidates on 5% of runs and switches on outcomes, cost and time.
