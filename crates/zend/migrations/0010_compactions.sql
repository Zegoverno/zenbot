-- Summaries of older turns (docs/context.md, compact.rs). Prepared in the background when a session
-- grows past the soft limit; applied later by appending a `compaction` block to the tape, which
-- holds the text the model reads. This table keeps how each was made.
CREATE TABLE compactions (
    id           BIGSERIAL PRIMARY KEY,
    session_id   UUID NOT NULL REFERENCES sessions(id),
    covers_from  INT NOT NULL,              -- first block summarized
    covers_to    INT NOT NULL,              -- last block summarized
    text         TEXT NOT NULL,             -- as the model reads it
    data         JSONB,                     -- the structured summary (goal, decisions, … with block refs)
    model        TEXT NOT NULL,             -- summarizer, or `fallback` (built without a model)
    usage        JSONB,
    cost_usd     DOUBLE PRECISION,
    error        TEXT,                      -- why the summarizer failed, when the fallback was used
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    applied_seq  INT                        -- the `compaction` block that applied it, once applied
);
CREATE INDEX compactions_session ON compactions(session_id, created_at);
