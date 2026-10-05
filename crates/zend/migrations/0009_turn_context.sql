-- What each turn sent the model, and whether it could reuse the previous turn's prompt cache
-- (docs/context.md, measure.rs).
ALTER TABLE turns
    ADD COLUMN envelope        TEXT,     -- the instructions and tools sent (envelopes.hash)
    ADD COLUMN context         JSONB,    -- what was sent: summary, history range and size, turn context, how
    ADD COLUMN context_tokens  BIGINT,   -- context size the provider reported for the turn's last model call
    ADD COLUMN cache_break     TEXT;     -- why the cache could not be reused, if it couldn't (null: it could)
