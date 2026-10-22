# zenbot — Progress Log

> Append-only. What shipped, what we learned, what changed and why. Newest entry at the top.
> Add an entry with every pull request that ships something; keep entries lean. Decisions go to
> DECISIONS.md, plans to ROADMAP.md.

---

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
