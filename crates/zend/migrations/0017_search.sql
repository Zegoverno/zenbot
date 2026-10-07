-- Search over sessions and memories (search.rs; DESIGN.md "Memory and knowledge" → Search). One table
-- for every kind of document, three ways in: exact names and paths (trigram), full text (`simple`
-- config, no stemming, so identifiers survive), and meaning (pgvector, filled in the background;
-- a row without an embedding is still found by the other two). Recipe: docs/research/memory-search-web.md §D.
CREATE EXTENSION IF NOT EXISTS vector;
CREATE EXTENSION IF NOT EXISTS pg_trgm;

CREATE TABLE search_docs (
    id          BIGSERIAL PRIMARY KEY,
    kind        TEXT NOT NULL,                 -- turn | memory (later wiki, skill)
    ref         TEXT NOT NULL,                 -- turn: <session>:<seq of its user message>; memory: m<id>
    session_id  UUID REFERENCES sessions(id),
    ident       TEXT,                          -- exact names: paths, ids (space-separated)
    title       TEXT,
    body        TEXT NOT NULL,
    tsv         TSVECTOR GENERATED ALWAYS AS (
                    setweight(to_tsvector('simple', coalesce(ident, '') || ' ' || coalesce(title, '')), 'A') ||
                    setweight(to_tsvector('simple', body), 'B')) STORED,
    embedding   vector(1536),
    embed_model TEXT,
    at          TIMESTAMPTZ NOT NULL DEFAULT now(),  -- when the document's content happened
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (kind, ref)
);
CREATE INDEX search_docs_tsv ON search_docs USING gin (tsv);
CREATE INDEX search_docs_ident ON search_docs USING gin (ident gin_trgm_ops);
CREATE INDEX search_docs_embedding ON search_docs USING hnsw (embedding vector_cosine_ops);
CREATE INDEX search_docs_unembedded ON search_docs (id) WHERE embedding IS NULL;

-- How far the indexer got (the last tape event, the last memory change).
CREATE TABLE search_state (
    key    TEXT PRIMARY KEY,
    value  TEXT NOT NULL
);

-- Every search: what was asked and what came back, to measure search and its reranking.
CREATE TABLE searches (
    id          BIGSERIAL PRIMARY KEY,
    session_id  UUID REFERENCES sessions(id),
    query       TEXT NOT NULL,
    scope       TEXT NOT NULL,
    results     JSONB NOT NULL,
    reranked    BOOLEAN NOT NULL DEFAULT false,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
