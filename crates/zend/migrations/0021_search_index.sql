-- Search indexing that keeps up (search.rs; DESIGN.md "Memory and knowledge" → Search). Expand-only:
-- `search_state`'s `memories` watermark and the `search_docs_unembedded` index are no longer used,
-- and stay until a later release.

-- Memories to (re)index. A trigger queues every memory whose text, tier or source changes (or that is
-- deleted) in the same transaction as the change, and the indexer deletes the queue rows it has
-- indexed. This replaces a watermark on `memories.updated_at`: `now()` is when a transaction
-- started, so a long one (the sleep) committing after a later, shorter one fell behind the
-- watermark and its changes were never indexed.
CREATE TABLE search_queue (
    id    BIGSERIAL PRIMARY KEY,
    kind  TEXT NOT NULL,       -- memory
    ref   BIGINT NOT NULL      -- memories.id
);

CREATE FUNCTION zen_queue_memory() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO search_queue (kind, ref) VALUES ('memory', CASE WHEN TG_OP = 'DELETE' THEN OLD.id ELSE NEW.id END);
    RETURN NULL;
END $$;

CREATE TRIGGER memories_search_insert AFTER INSERT OR DELETE ON memories
    FOR EACH ROW EXECUTE FUNCTION zen_queue_memory();
CREATE TRIGGER memories_search_update AFTER UPDATE OF text, tier, source ON memories
    FOR EACH ROW WHEN (OLD.text IS DISTINCT FROM NEW.text OR OLD.tier IS DISTINCT FROM NEW.tier OR OLD.source IS DISTINCT FROM NEW.source)
    EXECUTE FUNCTION zen_queue_memory();

-- Every memory once, so none the old watermark skipped stays out (or in, when archived).
INSERT INTO search_queue (kind, ref) SELECT 'memory', id FROM memories;

-- Embedding failures per document: a row the provider rejects is retried alone, and left without an
-- embedding (found by text only) after 3 tries; a change of its content resets the count.
ALTER TABLE search_docs ADD COLUMN embed_attempts INT NOT NULL DEFAULT 0;
ALTER TABLE search_docs ADD COLUMN embed_error TEXT;
CREATE INDEX search_docs_to_embed ON search_docs (id) WHERE embedding IS NULL AND embed_attempts < 3;

-- Skill loads by tool name (workshop stats and tend scan `tool_calls` for `load_skill`).
CREATE INDEX tool_calls_name ON tool_calls (name);
