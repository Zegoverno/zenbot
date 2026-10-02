-- Live scores: a System One model's answers to fixed questions about a session's work so far
-- (see score.rs). Kept alongside the session for later analysis; nothing acts on them.
CREATE TABLE session_scores (
    id               BIGSERIAL PRIMARY KEY,
    session_id       UUID NOT NULL REFERENCES sessions(id),
    turn_id          UUID REFERENCES turns(id),     -- the last turn scored
    scorer           TEXT NOT NULL,                 -- as configured, e.g. openrouter/typesafe/jev-1.13
    scorer_resolved  TEXT,                          -- as the provider reported it
    questions        TEXT NOT NULL,                 -- version of the question set
    trigger          TEXT NOT NULL,                 -- decision | idle
    answers          JSONB,
    usage            JSONB,
    cost_usd         DOUBLE PRECISION,
    error            TEXT,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX session_scores_session ON session_scores(session_id, created_at);
