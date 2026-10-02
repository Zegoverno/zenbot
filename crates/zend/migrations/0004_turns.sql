-- One row per turn (a prompt and everything the model did for it), with what produced it:
-- the harness (zenbot build), the engine and its version, the model and the thinking level.
CREATE TABLE turns (
    id              UUID PRIMARY KEY,
    session_id      UUID NOT NULL REFERENCES sessions(id),
    harness         TEXT NOT NULL,
    worker          TEXT NOT NULL,
    engine          TEXT,
    engine_version  TEXT,
    model           TEXT NOT NULL,      -- as requested, e.g. claude/claude-opus-5-5
    model_resolved  TEXT,               -- as the provider reported it
    effort          TEXT,
    started_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    ended_at        TIMESTAMPTZ,
    duration_ms     BIGINT,
    outcome         TEXT,               -- ok | error | interrupted
    error           TEXT,
    model_calls     INT,
    tool_calls      INT,
    tool_errors     INT,
    input_tokens    BIGINT,
    output_tokens   BIGINT,
    cache_read      BIGINT,
    cache_write     BIGINT,
    cost_usd        DOUBLE PRECISION,
    usage           JSONB               -- the engine's own turn report (e.g. per-model breakdown)
);
CREATE INDEX turns_session ON turns(session_id, started_at);

-- Calls recorded from now on belong to a turn. Older rows keep turn_id NULL.
ALTER TABLE model_calls ADD COLUMN turn_id UUID REFERENCES turns(id), ADD COLUMN duration_ms BIGINT;
ALTER TABLE tool_calls ADD COLUMN turn_id UUID REFERENCES turns(id);
CREATE INDEX model_calls_turn ON model_calls(turn_id);
CREATE INDEX tool_calls_turn ON tool_calls(turn_id);
