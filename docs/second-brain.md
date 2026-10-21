# Memory, knowledge and skills

The second brain has three curated stores. Memory preserves selected experience; knowledge is
sourced understanding; skills describe reliable execution. People, organizations and projects are
shared identities. A meeting links to people and commitments; a relationship brief assembles those
records. Session history is separately searchable evidence, not automatically promoted memory.

## Storage and organization

Postgres owns memory, revisions and work receipts. Private projections live in `global/MEMORY.md`,
`global/memory/records/`, `global/memory/archive/` and `global/memory/export.json`. Generated files
are edited with revisioned operations, not by silently changing a second authoritative copy.
Knowledge/entity Markdown is authoritative once published; database indexes and graph edges are
projections. File hashes are checked before reads; manual edits require explicit import. A backup
needs both the database and files. A search-index rebuild is not a complete backup restore.

Knowledge uses shallow folders by type under the configured wiki root: concepts, claims, arguments,
questions, syntheses, sources, events and subjects. Entities live under `global/entities/people/`,
`organizations/` and `projects/`. One canonical ID per record, with aliases and multiple subjects.
Subjects provide intellectual classification without duplicate pages. Existing flat wiki pages
remain available through the existing `capture`, `read` and `search` tools.

Frontmatter is a JSON object, a valid YAML subset, delimited by `---`; the body is Markdown:

```markdown
---
{"schema_version":1,"id":"system-one","store":"knowledge","kind":"concept",
 "title":"System 1 in Kahneman's framework","revision":1,"status":"provisional",
 "metadata":{"subjects":["cognitive-psychology"],"aliases":[],
 "evidence":[{"source":"reading-source","locator":"inspected introduction","basis":"interpretation"}],
 "links":[{"relation":"part_of","target":"dual-process-cognition"}]}}
---

A label for relatively automatic processing within this account. It is not an anatomical organ.
```

Use `zen brain schema` for accepted types and relationships. Evidence identifies source, locator
and basis: owner statement, observation, external claim, inference, proof, empirical evidence,
hypothesis, interpretation or tradition. These describe evidence, not automatically established
truth. Support/contradiction edges require evidence. Links may target `record-id#claim-anchor` with an
explicit `{#claim-anchor}` in the body. Citations to registered records retain source revisions. A model cannot establish source authenticity
merely by writing a citation. Preserve qualifications and competing claims in the body.

## Memory and sleep

`remember` saves selected recent experience. Source `owner` requires an exact span from an
actual authenticated owner message; an assistant's attribution is not evidence. Automatic capture
classifies exact owner-message spans rather than inventing paraphrased preferences. Captures have
stable retry keys. Explicit pins go directly to long-term memory.

The short-term soft target is `ZEN_MEMORY_CHARS` (4,000 characters); its double is the hard view
limit. Full buffers save pending entries durably. Sleep uses bounded S1 batches, keeps uncertainty,
promotes durable experience and archives clearly expired detail. Changed revisions are left alone.
Stale long-term/archive projections are withdrawn after disposition changes.
No automatic sleep writes USER/IDENTITY/SOUL/AGENTS. Model outages retain pending work; inspect
failed receipts and retry them. Archive is retention outside normal recall, not physical erasure.

Corrections and merges preserve prior versions and writer dependencies. A merge needs both current
revisions. `annotate` adds structured details and event time: record commitment owner/state/due date,
meeting participants or decision alternatives/revisit conditions there. The meeting transcript itself
is a source record or retained attachment; a short-memory summary is not its replacement.

## Owner commands

`zen brain ACTION` prints JSON. Supply structured arguments with `--args JSON`, or `--args -`
from stdin. `--id` and `--query` are shortcuts. Examples:

```bash
zen brain schema
zen brain list --args '{"store":"entity","offset":0}'
zen brain get --id ana-partner
zen brain brief --id ana-partner
zen brain neighbors --id pilot-meeting
zen brain lint
zen brain import --id pilot-project
zen brain repair --id pilot-project
zen brain correct --id m12 --args '{"expected_revision":2,"text":"The pilot is now paused."}'
zen brain history --id m12
zen brain annotate --id m12 --args '{"expected_revision":3,"details":{"state":"open","owner":"Jose"}}'
zen brain work
zen brain retry
zen brain skill_evidence
```

`put` takes `record` with id, store, kind, title, body and metadata. Updates require
`expected_revision`. `pin`, `promote`, `archive`, `correct`, `annotate`, `merge` and `undo` operate
on memory IDs; `undo` also takes the historical `revision`. Merge adds `merge_id`, `merge_revision`
and a concise merged `text`. Work, retry and revision history are owner operations.

`dream` returns tentative connections without publishing them. `synthesize` takes `session`,
`ids` (1–8 source records) and `query`; it queues a tool-free subscription-model proposal. Inspect
its result/usage with `work`; publish a reviewed record with `put`, or ask the agent to save a skill
draft. Proposed conclusions are not activated knowledge, skills or user preferences.

## Retrieval and inspection

`search` includes curated graph records, existing wiki, memory and session archives. Identifier,
English/Portuguese lexical and semantic arms complement each other. Semantic search still depends
on the configured embedding model's languages and quality. No numerical search score means truth.

Per-turn preparation uses recent dialogue, current memory, lexical/semantic candidates, typed
neighbors and skill pointers. It fetches record text, checks current revisions/provenance and uses
S1 to select evidence under a deadline. The default conservative evidence budget is 4,000 UTF-8
bytes (`ZEN_BRAIN_CONTEXT_BYTES`); a partial record is labelled, and full records remain retrievable.
Dynamic evidence uses the existing per-turn context field, preserving the fixed instruction prefix.

`zen brain retrieve --args '{"session":"SESSION_UUID","query":"What did Ana promise?"}'`
returns a context pack and receipts. `GET /api/sessions/{id}/context` shows recent selected
IDs/revisions, omissions, degradation and elapsed time. `ZEN_BRAIN_MODE=shadow` records retrieval
without injecting it or automatically capturing; `off` disables automatic retrieval/capture.

Coverage is explicitly unverified: top-K search and a bounded graph expansion cannot establish
complete context. Read original evidence for agreements, temporal questions or consequential action;
use paginated `list` for exhaustive inventories. Long records may be partially represented. When
providers fail, lexical retrieval remains; failures of admission withhold content.

## Skills and boundaries

Skills retain SKILL.md folders and progressive loading. Loads record the content digest and the
owner's accepted-session verdict is associated with those uses. New skills and revisions are drafts;
only explicit fingerprint review activates them. Earlier active content is archived before replacing
it. Loading a skill grants no execution permission, and a successful session is not proof every
step of a skill worked. Existing made-tool execution remains compatible. New authoring is retired by default;
`ZEN_LEGACY_TOOL_AUTHORING=1` is a migration switch. Prefer skills and supporting scripts.

Source-risk/contamination rules apply across memory and graph reads, merges, exports and context.
Private model calls obey `ZEN_S1_PRIVATE`. A correction cannot remove text a running model already
saw; original evidence remains in the tape. No automatic outreach, reminders or external ingestion
is implied by a relationship record. SQLite migration and image/PDF reading belong to later phases.
