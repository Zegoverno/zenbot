-- S1 (D-066): one ledger for every System One decision, its outcomes, the learner's calibration
-- and proposals, and the masked states `zen s1 compare` re-asks (kept 30 days). Expand-only.
ALTER TABLE decisions ADD COLUMN spec_sha text;
ALTER TABLE decisions ADD COLUMN spec_version integer;
ALTER TABLE decisions ADD COLUMN rung integer;
ALTER TABLE decisions ADD COLUMN latency_ms integer;
ALTER TABLE decisions ADD COLUMN cost_usd double precision;
ALTER TABLE decisions ADD COLUMN tokens bigint;
-- model | unavailable | blocked
ALTER TABLE decisions ADD COLUMN source text;
ALTER TABLE decisions ADD COLUMN items integer;
CREATE INDEX decisions_point ON decisions(point, created_at);

CREATE TABLE s1_states (
    decision_id bigint PRIMARY KEY REFERENCES decisions(id) ON DELETE CASCADE,
    state jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX s1_states_created ON s1_states(created_at);

-- What really happened, from a source of truth: the owner's queue answers and job choices, a
-- second model, a decision file's test case. `correct`: whether the decision's outcome was right;
-- `truth`: the outcome it should have had, when known.
CREATE TABLE s1_outcomes (
    id bigserial PRIMARY KEY,
    decision_id bigint NOT NULL REFERENCES decisions(id) ON DELETE CASCADE,
    correct boolean NOT NULL,
    truth text,
    source text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (decision_id, source)
);

-- The learner's calibration curve per decision, model and yes/no question: points (raw, calibrated).
CREATE TABLE s1_calibration (
    point text NOT NULL,
    model text NOT NULL,
    question text NOT NULL,
    curve jsonb NOT NULL,
    labelled integer NOT NULL,
    fitted_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (point, model, question)
);

-- Changes the learner proposes to a decision file; applied only on the owner's approval.
CREATE TABLE s1_proposals (
    id bigserial PRIMARY KEY,
    point text NOT NULL,
    kind text NOT NULL,
    summary text NOT NULL,
    change jsonb NOT NULL,
    status text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','approved','rejected','stale')),
    created_at timestamptz NOT NULL DEFAULT now(),
    decided_at timestamptz
);
CREATE UNIQUE INDEX s1_proposals_open ON s1_proposals(point, kind) WHERE status = 'pending';

-- The task check that held an action, so the owner's answer labels it.
ALTER TABLE action_gates ADD COLUMN s1_decision bigint;
