-- Briefed work (docs/brief.md). A session's state (framing, working, verifying, reported, closed,
-- open) is recorded on the tape (`state` blocks) and cached here. Verifier sessions are children
-- of the session they check (`kind` = verifier) and are hidden from the session list.
ALTER TABLE sessions
    ADD COLUMN state  TEXT,
    ADD COLUMN parent UUID REFERENCES sessions(id),
    ADD COLUMN kind   TEXT;

-- Who gave a verdict: the owner (ground truth) or the model (auto-close).
ALTER TABLE session_decisions ADD COLUMN source TEXT NOT NULL DEFAULT 'owner';

-- System One decisions (route, kind of work, model, unverified claims, the `decide` tool): what was
-- asked, the answer and its probability, whether it was acted on, and what actually happened.
CREATE TABLE decisions (
    id           BIGSERIAL PRIMARY KEY,
    session_id   UUID REFERENCES sessions(id),
    point        TEXT NOT NULL,            -- route | work | model | claim | tool
    model        TEXT,                     -- the System One model, or `policy`
    input        JSONB,
    answer       JSONB,
    chosen       TEXT,
    probability  DOUBLE PRECISION,
    acted        BOOLEAN NOT NULL DEFAULT false,
    actual       TEXT,                     -- what happened (the model's route, the owner's override, …)
    actual_by    TEXT,
    error        TEXT,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved_at  TIMESTAMPTZ
);
CREATE INDEX decisions_session ON decisions(session_id, created_at);

-- The routing policy: which model and thinking level do the work, per kind of work. Versioned; the
-- latest row is in force. Day-to-day changes are made by the improvement loop, logged here.
CREATE TABLE policies (
    version     SERIAL PRIMARY KEY,
    data        JSONB NOT NULL,            -- { "routes": { "<work>": { "model", "effort" } } }
    reason      TEXT,
    created_by  TEXT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
