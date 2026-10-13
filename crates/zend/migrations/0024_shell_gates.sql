-- D-056: reading marks provenance; only a held action revokes capabilities.
ALTER TABLE sessions ADD COLUMN risky_at TIMESTAMPTZ;
UPDATE sessions SET risky_at = tainted_at WHERE tainted_at IS NOT NULL;
CREATE TABLE action_gates (
    id BIGSERIAL PRIMARY KEY,
    session_id UUID NOT NULL REFERENCES sessions(id),
    action TEXT NOT NULL,
    reason TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    allowed_at TIMESTAMPTZ,
    used_at TIMESTAMPTZ
);
CREATE INDEX action_gates_session ON action_gates (session_id, id DESC);
