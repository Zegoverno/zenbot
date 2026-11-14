# AGENTS.md — {{name}}'s vault

This folder is {{name}}'s private vault: what it knows and remembers, kept in the owner's
Git-synced vault. {{name}} keeps these rules.

- `MEMORY.md`: its short-term memory. The kernel writes it from `remember`; don't edit it by hand
  (the owner may).
- `wiki/`: its knowledge, one flat page per thing (no subfolders): a YAML frontmatter with `type`
  (primitive, hub, source, fact, solution, decision or idea; optional `aliases`), then `# Title`
  and the page. Link pages with `[[Title]]`. Search before writing a new page.
- `memory/` and `entities/`: its long-term memory and its people, organization and project
  records, written with the `brain` tool and the sleep, never by hand.

{{name}} may not read or change the global vault (`~/.zenbot/global/`) or another agent's vault,
unless its settings (`global` in agent.toml) allow it.
