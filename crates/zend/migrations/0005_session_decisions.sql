-- The owner's decision on the work in a session so far (zen's /done): the ground truth that
-- live scores are compared against.
CREATE TABLE session_decisions (
    id          BIGSERIAL PRIMARY KEY,
    session_id  UUID NOT NULL REFERENCES sessions(id),
    turn_id     UUID REFERENCES turns(id),          -- the last turn the decision covers
    decision    TEXT NOT NULL CHECK (decision IN ('accept', 'more', 'reshape', 'drop')),
    note        TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX session_decisions_session ON session_decisions(session_id, created_at);
