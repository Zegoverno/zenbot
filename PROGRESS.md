# zenbot — Progress Log

> Append-only. What shipped, what we learned, what changed and why. Newest entry at the top.
> Add an entry with every pull request that ships something; keep entries lean. Decisions go to
> DECISIONS.md, plans to ROADMAP.md.

---

## 2026-10-22 — Phase 6: MCP sign-in, script jobs, the learning router (D-072 to D-074)

`zen mcp-login` / `/mcp login` sign in to remote MCP servers by pasting back the browser's address.
A report that can't be delivered no longer stops the schedule. Script jobs run a command with no
model; one zenbot writes waits for the owner's approval, and each run logs its command and exit
code. The learning router picks each unpinned session's, job run's and subtask's model by its kind
of task (System One), tries candidates on 5% of runs and switches on outcomes, cost and time.

## 2026-10-21 — Phase 5: tools see images and PDFs, batch edits, bounded output, fuller MCP (D-071)

`read` shows images and selected PDF pages (text and a rendered page) to both engines; `edit`
applies up to 100 replacements atomically and returns a diff. Shell and MCP output past 12 KB is
previewed head and tail with the full text saved, bounded to 256 MB in all. MCP calls no longer wait
on unrelated servers; HTTP replies are read as they stream; structured results, images, resources,
prompts, refresh-token OAuth and owner-answered forms (`zen mcp-pending`, `zen mcp-respond`) work,
checked against a FastMCP reference server. Tool descriptions are shorter.
