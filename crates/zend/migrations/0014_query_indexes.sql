-- Indexes for the kernel's frequent per-session lookups (expand-only: nothing is dropped).
-- A session's blocks of some kinds, in order (tape::load, the history tool, the latest summary).
CREATE INDEX IF NOT EXISTS tape_events_kind ON tape_events(session_id, kind, seq);
-- Whether a turn was already scored for a trigger (scoring, the idle loop).
CREATE INDEX IF NOT EXISTS session_scores_turn ON session_scores(turn_id, trigger);
-- A session's cost: model calls from before turns were recorded have no turn_id.
CREATE INDEX IF NOT EXISTS model_calls_session_untraced ON model_calls(session_id) WHERE turn_id IS NULL;
