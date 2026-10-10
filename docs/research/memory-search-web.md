# Memory, search and web: research for the zenbot redesign (2026-10-06)

> Dated research snapshot (2026-10-06), kept as history. Its facts about zenbot were true then and
> have changed since (zend now has `reqwest`; migration `0017_search.sql` adds search). What zenbot
> does today is in [DESIGN.md](../../DESIGN.md) and [MAP.md](../../MAP.md). The clones under `research/` were not kept.

Scope: DESIGN.md "Target design / Memory and knowledge", ROADMAP.md Phases 1–4. Sources are cloned
(shallow or sparse) under `research/`; paths below are relative to it. Nothing in the zenbot repo was edited.

Facts about zenbot checked here:
- `crates/zend/Cargo.toml` has **no HTTP client**. `reqwest 0.12` (rustls, json) exists only in
  `crates/zen/Cargo.toml`. axum already brings hyper/tokio, so reqwest in zend adds little.
- The Postgres image is `pgvector/pgvector:pg16`, with **pgvector 0.8.6** and **pg_trgm 1.6**
  available (checked in the running container). No migration creates `vector` or `tsvector` yet.

---

## 1. Letta: memory tiers and sleep-time compute

**Repo status.** `letta-ai/letta` `main` is now a landing page. Its `AGENTS.md` says the V1 server
is on the `archive` branch "for historical reference only" and must not be used to infer current
behavior. The current code is `letta-ai/letta-code` (TypeScript). V1 is cited below only for the
design history of the tiers.

**V1 tiers** (`letta-v1/constants.py`, `archive` branch, `letta/functions/function_sets/base.py`):
- *Core*: labelled blocks (`persona`, `human`, …) always in the prompt, each with a character `limit`
  (`schemas/block.py`: `limit = CORE_MEMORY_BLOCK_CHAR_LIMIT`; persona/human 20,000 in the last V1,
  2,000 in early MemGPT). An edit that goes over the limit fails, so the model has to rewrite the
  block shorter (`core_memory_replace`, `memory_rethink`).
- *Recall*: the full message history in the database, searched with `conversation_search`.
- *Archival*: embedded passages in a vector store (`archival_memory_insert` / `_search`).
- *Eviction*: summarization starts at `SUMMARIZATION_TRIGGER_MULTIPLIER = 0.9` of the context
  window. A warning (`MESSAGE_SUMMARY_WARNING_STR`) tells the agent to save to core or archival
  memory first. The oldest messages are then replaced by a recursive summary and stay in recall.
- *Sleep-time agents* (`letta-v1/sleeptime.py`): a group pairs the chat agent with a sleep-time
  agent. Every `sleeptime_agent_frequency` turns (`turns_counter % frequency == 0`), the sleep-time
  agent runs in the background over the messages since `last_processed_message_id`. It has only
  memory-editing tools (`BASE_SLEEPTIME_TOOLS = memory_replace, memory_insert, memory_rethink,
  memory_finish_edits`) and rewrites the shared core blocks. The chat agent loses its own core-edit
  tools (`BASE_SLEEPTIME_CHAT_TOOLS`) and keeps only search. Letta's blog says this lets a cheap chat
  model work alongside a stronger memory model. The paper (Lin et al. 2025, "Sleep-time Compute")
  reports about 5× less test-time compute for the same accuracy on stateful GSM/AIME, and lower
  cost per query when several queries share one context.

**Current letta-code** (`letta-code/src/agent/…`):
- Memory is a git repo, "MemFS". A root `MEMORY.md` is the index. Other root `.md` files are
  *core* (always in context). Child directories are *deferred* (only names and descriptions are
  visible; content is read on demand). `skills/` holds procedures.
- Limits are enforced by a pre-commit validator (`memory-constraints.ts`): `maxFileCharacters
  20_000`, `maxCoreMemoryCharacters 65_536`, `maxDepth 2` (`src/memory-constraints.ts:302`).
- "Dreaming" is the reflection subagent (`subagents/builtin/reflection-v2.md`). It is triggered
  `step-count` (default `DEFAULT_STEP_COUNT = 25`, `src/reflection-settings.ts`) or by a
  `compaction-event` (`src/cli/helpers/post-turn-reflection.ts`). Its phases are Investigate →
  Extract → Update → Review → Commit. Its filters: "Lasting or ephemeral? Already captured?
  Generalizable? Temporal references → absolute dates", and "memory or skill?". It prefers
  edit/extend over create ("when unsure between create and none, choose none"). It fixes
  contradictions at the source instead of appending, moves retired content to `ARCHIVE.md`, and
  allows at most one skill operation per run. Every change is a git commit with trailers, so it can
  be reverted.

**Takeaways for zenbot.** Letta enforces the fixed size hard (a write over the limit fails). Its
consolidation job is an agent with only memory tools, run in the background on a counter. Memory
lives in git and is reverted by commit. Zenbot's design differs on purpose: System One *scores* and
the kernel decides deterministically. Letta has no probability threshold or lower confidence bound
anywhere; its consolidation runs on the model's judgment alone.

## 2. Karpathy LLM Wiki and gbrain

**Karpathy's gist** (gist 442a6bf…): three layers, *raw sources* (immutable), *the wiki*
(LLM-maintained markdown) and *the schema* (a config document saying how the wiki is organised).
There are three operations. *Ingest* reads a source, writes a summary and updates the entity and
concept pages it touches ("a single source might touch 10–15 pages"). *Query* answers with
citations, and good answers become pages. *Lint* finds contradictions, stale claims, orphans and
missing links. Two special files: `index.md` (a catalogue with a one-line summary per page, read
first on every query) and `log.md` (append-only, parseable `## [2026-04-02] ingest | Title`).
Search (BM25 + vector, e.g. qmd) only becomes necessary once the index outgrows the context.

**gbrain** (`gbrain/`, public, Garry Tan; runs about 156k pages):
- *Compiled truth + timeline* (`docs/GBRAIN_RECOMMENDED_SCHEMA.md` §2). Above a `---`: an executive
  summary, State fields, Open Threads, See Also, "always rewritten when new information arrives".
  Below it: `## Timeline`, "append-only, never rewritten", reverse-chronological,
  `- **YYYY-MM-DD** | Source — What happened.` A resolved open thread moves into the timeline.
- Empty sections are kept as `[No data yet]`, because "the structure itself is a prompt for future
  enrichment".
- Epistemics: every claim cites a source, labelled `observed` / `self-described` / `inferred`.
  Confidence follows the number of observations. "Never generalize from a single data point" (that
  belongs in the timeline). The owner's corrections override everything.
- Underneath, an event ledger and a fact store, with the page as a generated view. Contradictions
  are stored as two facts for the same field, so they become data instead of bugs.
- A resolver (`RESOLVER.md`) gives one primary home per entity (MECE directories). Merging a
  duplicate merges its timeline into the survivor in chronological order (schema doc l.200).
- The nightly "dream" cycle (`src/core/cycle.ts`): lint --fix → backlinks --fix → sync →
  synthesize (transcripts → pages) → extract links → patterns (cross-session themes) → embed stale
  → orphans report. It runs under a Postgres lock row with a 30-minute TTL.
- Core memory (`docs/guides/core-memory.md`): pages with `always_load: true` and `core_priority`
  render only their compiled truth, never the timeline. It is **off by default**: in their held-out
  eval, always loading a preferences page "helped one model and hurt another". This argues for
  zenbot measuring `MEMORY.md`'s effect instead of assuming it helps.

## 3. Hybrid search in Postgres

**gbrain's production recipe** (`src/core/search/hybrid.ts`, `postgres-engine.ts`, `schema.sql`):
- A keyword arm uses `search_vector @@ websearch_to_tsquery(lang,$1)` ranked by `ts_rank` (summary
  weighted `B`, timeline `C`). A vector arm uses HNSW `vector_cosine_ops`. `RRF_K = 60`, score =
  Σ 1/(60+rank).
- After fusion: a 2.0× boost for compiled-truth chunks, then a blend of 0.7·RRF + 0.3·cosine, then
  dedup. There is a pre-fusion pool floor so small limits don't starve fusion (D-3002).
- Exact identity comes before ranking (`exact-lookup.ts`): a slug, an exact title or an alias is
  promoted to rank 1 or injected, so "no amount of RRF/rerank scoring should be able to bury that
  page". Superseded pages are never promoted. There is a trigram GIN index on titles.

**Tokenizer facts, checked on zenbot's own Postgres** (`ts_debug('simple', …)`):
- `crates/zend/src/compile.rs` becomes **one `file` token**. So `websearch_to_tsquery('simple',
  'compile.rs')` does **not** match the path, while the full path does. Partial paths need trigram
  or `ILIKE`.
- `system_prompt` splits into `system`, `prompt` (it still matches as a phrase). `D-026` becomes
  `'d' <-> '-026'` (matches). `ZEN_WORKERS` becomes `zen`, `workers`.
- Conclusion: full-text search alone can't do "exact names and paths first". It needs its own
  exact/trigram arm.

**Embeddings** (OpenRouter `GET /api/v1/embeddings/models`, prices per million tokens):
`openai/text-embedding-3-small` $0.02 (1536 dims, `dimensions` can shrink it, 8k context);
`qwen/qwen3-embedding-8b` $0.01 (4096 dims native, 32k context); `qwen3-embedding-4b` $0.02;
`voyageai/voyage-4-lite` $0.02; `baai/bge-m3` $0.01 (1024 dims, multilingual, 8k);
`openai/text-embedding-3-large` $0.13 (3072). Free: `nvidia/nemotron-3-embed-1b:free`.
pgvector HNSW indexes at most **2000 dims for `vector`** and 4000 for `halfvec`, so 4096-dim Qwen
needs truncation (MRL) or `halfvec`.

## 4. Web search providers

| Provider | Request | Auth | Free / price (Oct 2026) | Notes for agents |
|---|---|---|---|---|
| Brave | `GET api.search.brave.com/res/v1/web/search?q=&count≤20` → `web.results[{title,url,description,age,extra_snippets}]` | `X-Subscription-Token` | Free tier retired in Feb 2026. **$5 of credit a month ≈ 1,000 queries**, then $5 per 1k, 50 rps | Own independent index; snippets plus an "LLM context" endpoint; privacy-friendly |
| Tavily | `POST api.tavily.com/search {query, search_depth basic\|advanced, max_results, include_answer, include_raw_content}` → `results[{title,url,content,score}]` | Bearer | **1,000 credits a month free**, $0.008 per credit after (advanced = 2) | Built for agents; returns cleaned content; can answer directly |
| Exa | `POST api.exa.ai/search {query, type: auto\|fast\|deep, numResults, contents:{text,highlights}}` | `x-api-key` | $7 per 1k (10 results); Deep $12–15 per 1k | Neural search, strong for "find pages like…"; also a keyless MCP (`mcp.exa.ai`) |
| SearXNG | `GET <instance>/search?q=&format=json` (JSON must be enabled in `settings.yml`) → `results[{title,url,content,engine}]` | none (self-hosted) | free; one Docker container | Metasearch over Google, Bing, DuckDuckGo and others; quality varies, upstreams sometimes rate-limit |
| DuckDuckGo | no official API; HTML scraping (`ddgs` package) | none | free | Unofficial and breaks under CAPTCHA or rate limits. OpenClaw calls it "unofficial HTML-based" |

**How the reference projects do it.** Hermes (`hermes-agent/agent/web_search_provider.py`) uses a
`WebSearchProvider` ABC with `is_available()` (no network), `search(query, limit) →
{success, data:{web:[{url,title,description,position}]}}` and an optional `extract`. Auto-detection
goes tavily → … → searxng → brave-free → ddgs. A *one-shot keyless rescue*
(`tools/web_tools_rescue.py`) sends a single failed keyed call through free tiers and never caches
the rescued result. OpenClaw (`oc/docs/tools/web.md`) normalises results to `{kind:"results",
provider, query, results[{title,url,snippet,published?,siteName?}], externalContent:{untrusted:true,
wrapped:true}}`. Provider text is re-wrapped exactly once at the boundary, so a provider can't
spoof the trust marker. URLs must parse as http(s) and `published` must look like an ISO date.
Brave comes first in its auto-detect order.

## 5. web_fetch

**Rust crates** (crates.io, 2026-10-06; *recent* = downloads in the last 90 days):

| Crate | Version / updated | Recent dl | Deps | Fit |
|---|---|---|---|---|
| `dom_smoothie` | 0.18.2 / 2026-09-21 | 429k | dom_query (html5ever), phf, tendril, … | Port of Mozilla readability.js, maintained; `TextMode::Markdown`, `is_probably_readable`, a policy for when strict extraction fails |
| `readability` | 0.3.0 / 2023-12 | 460k | html5ever, regex, optional reqwest | Unmaintained since 2023 |
| `readabilityrs` | 0.1.4 / 2026-08 | 115k | scraper, regex, serde | Young |
| `llm_readability` | 0.0.17 / 2026-04 | 13k | spider ecosystem | Small user base, pre-1.0 |
| `htmd` | 0.5.5 / 2026-07 | 4.2M | html5ever, markup5ever_rcdom, phf | Turndown-style HTML→Markdown, light, popular |
| `fast_html2md` | 0.0.63 | 104k | spider | Pre-1.0 |
| `html2text` | 0.17.1 / 2026-04 | 1.6M | html5ever | Text rendering for terminals, not Markdown |
| `scraper` | 0.27.0 | 9.1M | html5ever, selectors | Selectors only (links, title) |
| `pdf-extract` | 0.12.1 / 2026-09 | 3.1M | lopdf, … | PDF text; heavier and uneven on complex PDFs |

**How others handle limits and safety:**
- **Claude Code WebFetch** (from its tool contract, as seen in this session): HTTP is upgraded to
  HTTPS. Cross-host redirects are *returned to the model, not followed*. It fails on localhost and
  dotless hosts. A 15-minute cache per URL. The page is converted to markdown and a small fast
  model answers a `prompt` over it. Long pages are paged with an `offset` (this session read a
  100k-character window of a 136k page). Binary content such as PDFs is saved to a local file in the
  session, with the content treated as untrusted. Domain permissions apply.
- **OpenClaw** (`oc/docs/tools/web-fetch.md`): defaults `maxChars 20000` (also the hard cap),
  `maxResponseBytes 750000` (range 32k–10M), `timeoutSeconds 30`, `maxRedirects 3`,
  `cacheTtlMinutes 15`, and `ssrfPolicy.dangerouslyAllowPrivateNetwork: false`. It "blocks
  private/internal hostnames and re-checks redirects", sends a Chrome-like UA, runs Readability and
  falls back to Firecrawl when Readability fails. The result includes `finalUrl, status,
  contentType, truncated, rawLength, externalContent`, and truncated text spills to a private file.
- **Hermes** (`hermes-agent/tools/url_safety.py`, `web_tools_truncate.py`): cloud metadata is
  *always* blocked (169.254.169.254, 169.254.170.2, Azure 169.254.169.253, Alibaba 100.100.100.200,
  `fd00:ec2::254`, `metadata.google.internal`), with an opt-in for other private ranges. It blocks
  CGNAT 100.64/10 explicitly (Python's `is_private` misses it), checks IPv4 embedded in IPv6, closes
  DNS rebinding by re-checking at TCP connect and dialling the *validated IP*, and re-validates every
  redirect. Output is `DEFAULT_EXTRACT_CHAR_LIMIT = 15000`, clamped to 2k–500k, as a head+tail
  window. The full text (≤ 2M chars) is stored and paged with `read_file`.

---

## Recommendation for zenbot

### A. Sleep hygiene (Phase 1, step 4)

- **Budget:** `MEMORY.md` is 6,000 characters (about 1.5k tokens), the hard ceiling 12,000. For
  comparison: MemGPT started at 2,000 per block, and Letta's core memory is capped at 64k.
- **Questions to System One, per entry, in batches** (all logged in `decisions`):

| Key | Question | Output |
|---|---|---|
| `need` | Will the agent need this in the next 7 days? | p |
| `durable` | Will it still be true in 6 months? | p |
| `impact` | Does it change how the agent should act for the owner across jobs, not just one task? | p |
| `covered` | Is it already stated by another memory, a skill, `USER.md`, `SOUL.md` or `AGENTS.md`? Return the id if so | p + id |
| `about_owner` | Is it a fact or preference about the owner? | p |

- **Keep:** `rank = (1 − covered) · (0.5·need + 0.3·impact + 0.2·recency)`, where
  `recency = 2^(−days_since_last_used / 7)`. Fill the 6,000 characters from the top, with
  hysteresis: an incumbent leaves only if the entry replacing it ranks ≥ 0.05 higher, which stops
  nightly churn. Entries the owner pins are never dropped. `covered ≥ 0.9` with an id becomes a
  merge proposal, never a silent delete.
- **Promote (shadow in Phase 1), using the lower confidence bound.** A raw probability from System
  One is not a confidence. Turn each score into an LCB from **calibration data**:
  1. Bucket the System One probability (e.g. 0.90–0.95, 0.95–0.98, ≥ 0.98).
  2. For each bucket, keep `k/n`: shadow proposals that the owner later confirmed / all proposals
     the owner reviewed. Owner review comes from the morning note (accept, undo) or from the entry
     still being true and used 30 days later.
  3. `LCB = Wilson lower bound (one-sided 95 %, z = 1.645)` of k/n for that bucket.
  4. Promote only if `LCB(durable) ≥ 0.95 ∧ LCB(impact) ≥ 0.95`, the source is `owner` or
     `verified`, and `covered < 0.2`. If `about_owner` is high, write a proposed `USER.md` edit
     instead of promoting.
  - Consequence: with no data the LCB is low, so **nothing promotes on its own**. Even with
    perfect agreement, LCB ≥ 0.95 needs `n ≥ z²·0.95/0.05 ≈ 52` reviewed proposals in the bucket.
    That is exactly the "shadow until it matches the owner's calls often enough" rule in ROADMAP
    Phase 3, as one formula.
  - A cheaper add-on: ask each question twice (two paraphrases, or two System One models) and use
    the minimum. A disagreement over 0.2 sends the entry to the morning note instead of acting.
- **Drop:** `state = archived`, never deleted. Every action has an undo in the morning note.
- **Fixed nightly budget:** stop scoring at the token cap; unscored entries keep their previous
  scores. Record the cost in the job row. Entries that were not used and not changed since the last
  sleep reuse their scores, which saves most of the calls.
- **Ideas taken from letta-code's reflection filters,** for the `remember` tool description and the
  capture step: store lasting patterns, not events. Convert relative dates to absolute ones. Fix a
  contradiction at its source. Don't record what the tape already holds.

### B. Long-term memory schema (expand-only migration)

```sql
CREATE TABLE memories (
  id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  text         text NOT NULL CHECK (length(text) <= 1000),
  source       text NOT NULL CHECK (source IN ('owner','verified','inferred')),
  source_ref   jsonb,                    -- {session_id, turn, tape_seq} or {url}
  kind         text NOT NULL DEFAULT 'fact',     -- fact|preference|procedure_hint|project
  state        text NOT NULL DEFAULT 'short' CHECK (state IN ('short','long','archived')),
  pinned       boolean NOT NULL DEFAULT false,
  supersedes   uuid REFERENCES memories(id),     -- replace = new row + supersedes
  created_at   timestamptz NOT NULL DEFAULT now(),
  last_used_at timestamptz,
  use_count    int NOT NULL DEFAULT 0,
  scores       jsonb,                    -- latest {need,durable,impact,covered,lcb:{…},model,at}
  state_reason text                      -- 'sleep:2026-10-07 keep rank 0.71' etc.
);
CREATE INDEX memories_state ON memories(state) WHERE state <> 'archived';
```
History stays in `decisions` (one row per scoring and per action). `remember replace` writes a new
row with `supersedes` and archives the old one, so nothing is overwritten in place.

### C. Wiki page format (Phase 4), gbrain-style and matching SPEC.md §5.9

```markdown
---
type: concept            # entity|concept|decision|playbook|project|person
scope: zenbot            # sensitivity/visibility scope
aliases: [System One, s1]
sources: [session:…, url:…]
updated: 2026-10-06
confidence: medium       # from number/quality of timeline entries
---
# System One
> One-paragraph summary: if you read only this, you know the state of play.

## State
## Open threads
## See also        (wiki-links)

---
## Timeline
- **2026-10-06** | session 3f2a… (owner) — Decided X because Y.  [observed]
```
Rules: the timeline is append-only and newest first. Each entry is `date | source — what` with
`observed|owner|inferred`. The summary above `---` is regenerated from the timeline and never edited
by hand. One page per concept, chosen by `search` + System One; a merge appends the duplicate's
timeline. Keep Karpathy's `wiki/index.md` (one line per page) and `wiki/log.md` (`## [date] capture
| page`). Lint nightly in the sleep job: broken links, orphans, summaries older than their newest
timeline entry. Only the summary goes into search's boosted field.

### D. Hybrid search SQL (Phase 3)

One table over all kinds; embeddings are filled in asynchronously, and a row without one is
still found by full-text search.
```sql
CREATE EXTENSION IF NOT EXISTS vector; CREATE EXTENSION IF NOT EXISTS pg_trgm;
CREATE TABLE search_docs (
  id bigserial PRIMARY KEY,
  kind text NOT NULL,            -- turn|memory|wiki|skill
  ref text NOT NULL,             -- session_id:turn, memory id, wiki path, skill path
  ident text,                    -- exact names: path, slug, title, aliases (space-joined)
  title text, body text NOT NULL,
  tsv tsvector GENERATED ALWAYS AS (
        setweight(to_tsvector('simple', coalesce(ident,'')||' '||coalesce(title,'')), 'A') ||
        setweight(to_tsvector('simple', body), 'B')) STORED,
  embedding vector(1536), embed_model text,
  updated_at timestamptz NOT NULL DEFAULT now(), UNIQUE (kind, ref));
CREATE INDEX ON search_docs USING gin (tsv);
CREATE INDEX ON search_docs USING gin (ident gin_trgm_ops);
CREATE INDEX ON search_docs USING hnsw (embedding vector_cosine_ops);
```
Use `simple`, not `english`, because the owner writes in more than one language and stemming
mangles identifiers. Morphology is the vector arm's job; the lexical arm is for exact tokens.

```sql
-- $1 query text, $2 query embedding (NULL if unavailable), $3 exact-candidate (path/identifier or NULL),
-- $4 kinds filter text[], $5 limit
SET LOCAL hnsw.ef_search = 100; SET LOCAL hnsw.iterative_scan = relaxed_order;  -- pgvector ≥0.8, filters
WITH q AS (SELECT websearch_to_tsquery('simple', $1) AS tsq),
exact AS (
  SELECT id, row_number() OVER (ORDER BY (ident = $3) DESC, similarity(ident, $3) DESC) AS r
  FROM search_docs WHERE $3 IS NOT NULL AND kind = ANY($4)
    AND (ident = $3 OR ident ILIKE '%' || replace(replace($3,'%','\%'),'_','\_') || '%')
  LIMIT 10),
fts AS (
  SELECT id, row_number() OVER (ORDER BY ts_rank_cd(tsv, q.tsq, 32) DESC) AS r
  FROM search_docs, q WHERE tsv @@ q.tsq AND kind = ANY($4)
  ORDER BY ts_rank_cd(tsv, q.tsq, 32) DESC LIMIT 50),
vec AS (
  SELECT id, row_number() OVER () AS r FROM (
    SELECT id FROM search_docs WHERE $2 IS NOT NULL AND embedding IS NOT NULL AND kind = ANY($4)
    ORDER BY embedding <=> $2 LIMIT 50) v)
SELECT d.id, d.kind, d.ref, d.title,
       (e.id IS NOT NULL) AS exact_hit,
       coalesce(1.0/(60+f.r),0) + coalesce(1.0/(60+v.r),0) AS rrf,
       ts_headline('simple', d.body, (SELECT tsq FROM q), 'MaxFragments=2,MaxWords=30') AS snippet
FROM search_docs d
LEFT JOIN exact e USING (id) LEFT JOIN fts f USING (id) LEFT JOIN vec v USING (id)
WHERE e.id IS NOT NULL OR f.id IS NOT NULL OR v.id IS NOT NULL
ORDER BY exact_hit DESC, e.r NULLS LAST, rrf DESC LIMIT $5;
```
- **`$3` (exact candidate):** the kernel sets it when the query, or a quoted token in it, looks like
  a path or identifier (it contains `/ . _ :: -` with no spaces, or matches `D-\d+`).
- **Kind boosts:** add them after fusion, gbrain-style: wiki summary ×1.5, the current session's own
  turns ×1.2. Keep them small; at k = 60 a 2× boost already lets rank 60 beat rank 1.
- **Reranking:** System One reranks the top 20 and has the final say.
- **Logging:** each search writes `{query, arms' ids, chosen}` to a `searches` row.
- **sqlx:** pass the embedding as text `'[…]'::vector`, which needs no new crate, or add `pgvector`
  with its `sqlx` feature.
- **Chunks:** one `turn` row per turn (user + assistant text, tool calls summarized), split at about
  2,000 characters.

**Embedding choice:** `openai/text-embedding-3-small` through OpenRouter's OpenAI-compatible
`/api/v1/embeddings`, **1536 dims** (fits HNSW `vector`), $0.02 per million tokens, so the whole
history costs cents. It is multilingual enough and stable, and also reachable directly at OpenAI.
Store `embed_model` on each row so a later switch (e.g. `qwen3-embedding-8b` at $0.01, truncated to
1536) can re-embed in the background. Shadow-compare the two on logged searches before switching.

### E. `web_search` contract and default (Phase 2)

```rust
#[async_trait] trait SearchProvider: Send + Sync {
    fn name(&self) -> &'static str;
    fn available(&self) -> bool;                       // config/key present; no network
    async fn search(&self, q: &SearchQuery) -> Result<Vec<SearchHit>, SearchError>;
}
struct SearchQuery { query: String, count: u8 /*≤10*/, freshness: Option<Freshness>, site: Option<String>, lang: Option<String> }
struct SearchHit  { title: String, url: Url /*http(s) only*/, snippet: String, published: Option<NaiveDate>, provider: &'static str, rank: u16 }
enum SearchError  { RateLimited, Auth, Upstream(String), Timeout }
```
- **Tool output:** `{provider, query, results:[…], untrusted:true}`, with every provider string
  wrapped once by the kernel and marked `<untrusted source="web_search">`.
- **Ranking:** System One reranks or filters by relevance (DESIGN.md).
- **Caching:** 15 minutes per (provider, query).
- **Default: Brave.** It has its own index (not a Bing reseller), the cheapest per-query paid price,
  about 1k free queries a month through the $5 credit, structured snippets, and it comes first in
  OpenClaw's auto-detect order. Tavily is a good alternative when cleaned content per result is
  wanted.
- **Keyless fallback: SearXNG** as a second service in `deploy/compose.yaml`, bound to `127.0.0.1`
  with `format: json` enabled. It is self-hosted, needs no key and has no ToS ambiguity.
- **Rescue:** used when no key is configured, and **once per failed call** on `RateLimited`/`Auth`
  (Hermes's rescue pattern; the rescued result is never cached).
- **No DuckDuckGo:** it is unofficial scraping.

### F. `web_fetch` crate and safety rules (Phase 2)

- **Crates:** add `reqwest` to zend (the same `0.12`, `rustls-tls`, `stream`; reuse the workspace
  version) plus **`dom_smoothie`** (readability.js port, `TextMode::Markdown`). Use **`htmd`** as the
  fallback when `is_probably_readable` is false (full-page markdown). Links come from
  `dom_smoothie`'s `dom_query`: return them numbered with absolute URLs (`[3] text → url`) so the
  agent can follow `link: 3`.
- **PDFs:** don't add `pdf-extract` yet. Save the PDF into the session sandbox and tell the agent to
  use `pdftotext`. Revisit if PDFs turn out to be common.
- **SSRF:** only `http`/`https`. Reject userinfo in the URL. Use a **custom
  `reqwest::dns::Resolve`** that resolves with `tokio::net::lookup_host` and drops every non-global
  address. The connection then uses the validated IP, which closes DNS rebinding as Hermes does.
  IP literals are checked the same way. Block:
  - loopback, `0.0.0.0/8`, RFC 1918, `100.64/10` (CGNAT, which also covers Tailscale), and
    `169.254/16` (cloud metadata)
  - multicast and broadcast
  - `::1`, `fc00::/7`, `fe80::/10`, and IPv4-mapped or IPv4-compatible IPv6 (check the embedded v4)
  - names `localhost`, `*.internal`, `metadata.google.internal`
  
  Also call `.no_proxy()` so the proxy can't bypass the resolver. Redirects use
  `redirect::Policy::custom`: at most 5, and the scheme and host are re-checked on every hop (the
  resolver re-checks the IP). Return `final_url`. A private-network allow-list exists only in
  owner config.
- **Size and time:** connect timeout 10 s, total 30 s. Stream the body and stop at **5 MB** (OpenClaw
  750 KB by default; raise per content type). Output is **20,000 characters** by default (cap 100k).
  Beyond that, store the full text (≤ 2 M characters) in the kernel and return
  `{truncated, next_offset}`, paged by calling again with `offset`. Cache 15 minutes. An honest UA:
  `zenbot/<ver> (+repo URL)`.
- **Content types:** `text/html` → readability → markdown. `text/plain`, markdown and JSON are
  passed through with a size cap. `application/pdf` → sandbox file. Other binaries are refused with
  the type and size. Decode with the charset from the header or meta tag, falling back to UTF-8
  lossy.
- **Result:** `{url, final_url, status, content_type, title, text, links[], truncated, raw_bytes,
  fetched_at, untrusted:true}`. The optional `prompt` / "keep only relevant parts" goes to System One
  or a small model, as Claude Code does.
- **Taint (SPEC.md §5.18):**
  - The kernel wraps every `web_search` and `web_fetch` result in an untrusted envelope it creates
    itself. It strips look-alike markers from the page first, so a page can't spoof the envelope
    (OpenClaw's re-wrap-once rule).
  - Mark the session `tainted_at` (new column) and record it on the tape.
  - From then on: no secrets are injected, nothing is sent outward or posted, and tool calls
    receive no URLs with query strings built from session data, all without owner approval.
  - Taint is inherited by delegated subagents.
  - `remember` and `capture` from a tainted session get `source = inferred` at most, so web content
    can never promote itself into long-term memory.
