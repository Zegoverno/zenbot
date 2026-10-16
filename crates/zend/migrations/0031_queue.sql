-- Owner queue decisions are immutable and distinct from consuming a grant.
ALTER TABLE action_gates ADD COLUMN decision text CHECK (decision IN ('approved','rejected','denied'));
ALTER TABLE action_gates ADD COLUMN resolved_at timestamptz;
CREATE INDEX action_gates_pending ON action_gates(created_at DESC) WHERE decision IS NULL AND allowed_at IS NULL;
