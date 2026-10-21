# Memory, knowledge and skills

Three curated stores (D-069): **memory** (what matters from lived work: decisions and why,
commitments, preferences, open questions), **knowledge** (understanding of a subject and how we know
it, disagreements and uncertainty kept) and **skills** (how to do a task well). People,
organizations and projects are records all three point to. Session history is a searchable archive,
not memory. Everything is Markdown: the files are the source of truth, the database an index rebuilt
from them (`zen brain index`), so a backup of the files is a backup of the stores.

**Short-term memory** is `~/.zenbot/global/MEMORY.md`, capped (`ZEN_MEMORY_CHARS`, 4000) and in every
session's instructions. The agent saves with `remember` (a kind: experience, decision, preference,
commitment, question; `source` `owner` only for the owner's exact words); System One also keeps
sentences of the owner's own message after a turn (`memory_capture`). Past the size the sleep runs at
once. Edit the file in any editor: a changed line changes its entry, a new `- text` line adds one, a
removed line archives it. Entries saved after reading untrusted content are listed by id only.

**Records** (long-term memory, knowledge, entities) are one file each, metadata as a JSON object
(valid YAML) between `---` lines, then the body:

```markdown
---
{"id": "system-one", "store": "knowledge", "kind": "concept", "title": "System 1 in Kahneman's account",
 "status": "provisional", "revision": 1,
 "metadata": {"tags": ["cognition"], "subjects": ["cognitive-psychology"], "aliases": ["System One"],
  "evidence": [{"source": "thinking-fast-and-slow", "locator": "part 1", "basis": "interpretation"}],
  "links": [{"relation": "part_of", "target": "dual-process-cognition"}]}}
---

A label for relatively automatic processing within this account, not an anatomical organ.
```

| Store | Kinds | Folder |
|---|---|---|
| `memory` | experience, decision, preference, commitment, question | `global/memory/` |
| `entity` | person, organization, project | `global/entities/{people,organizations,projects}/` |
| `knowledge` | concept, claim, argument, question, synthesis, source, event, subject | `<wiki>/{concepts,claims,arguments,questions,syntheses,sources,events,subjects}/` |

One canonical id per record; `aliases` are other names, and a name belongs to one record. `subjects`
and `tags` classify across folders. Evidence has a `source`, a `locator` and a `basis`
(owner_statement, observed, external_claim, inference, proof, empirical, hypothesis, interpretation,
tradition): how something is known, not that it is true. Links are `{relation, target, evidence}`
(`zen brain schema` lists the relations); supports, contradicts and derived_from need evidence. A
meeting is an `event` linked `about` its people and project. The agent writes with `brain` put
(schema checked; a taken name, a missing target or a stale `expected_revision` refused; the old
version backed up in `backups/brain/`), never by editing the files.

**The sleep** (nightly at 03:00 UTC, when short-term memory is full, or `zen memory sleep`): System One
promotes each lasting memory to a long-term record citing it (`memory_durable`) and archives finished
ones (`memory_disposition`); the rest stay. `remember` with `long: true` goes long-term on the spot
only past a higher bar (`memory_long`). A memory changed meanwhile is left alone; one whose writer was
denied is archived. Nothing is deleted; prompt files are never touched. The bars are costs in
`crates/zend/s1/memory_*.toml` (overridable in `~/.zenbot/s1/`).

**Finding things.** Nothing is pushed into a turn: the brain uses `search` (the `work/search` skill).
Exact names first, then words (English and Portuguese stemming) and meaning, ranked by System One
(`search_rerank`). From each record it keeps, search follows typed links: a linked record joins when
kept too, two hops at most; `contradicts` is always followed one hop and a superseded record gives
way to the newer one. Results say how they were reached and list their links; `brain get` reads a
record with its links both ways. Owner commands: `zen brain schema|list|get|put|lint|index`
(arguments as JSON with `--args`), `zen memory`, `zen memory sleep`.

**Ratings.** A record's rating is its bytes' (D-067): the writer's and every cited memory's or
record's; the owner's own edits take the highest rating the file's writers had. Reading raises the
reader to it and records its writers as inputs; from 2 it is wrapped as untrusted, and a record whose
writer was denied is withheld. A short-term memory keeps every session that wrote or replaced it.

**Skills** stay `SKILL.md` folders. New skills and revisions of active ones are drafts until the owner
accepts them by fingerprint. New made tools are retired (`ZEN_LEGACY_TOOL_AUTHORING=1` re-enables them).
