-- How a session ended, set by the owner when they archive it (`archive done|partial|not-done
-- [note]`): the learning loop's ground truth. NULL when archived without one, or still open.
-- Replaces session_decisions (accept|more|reshape|drop), which is kept as history but no longer written.
ALTER TABLE sessions ADD COLUMN outcome TEXT CHECK (outcome IS NULL OR outcome IN ('done', 'partial', 'not-done'));
ALTER TABLE sessions ADD COLUMN outcome_note TEXT;
ALTER TABLE sessions ADD COLUMN ended_at TEXT;
-- A session started to continue a `partial` one (handoff.rs): the session it continues.
ALTER TABLE sessions ADD COLUMN continues BLOB CHECK (continues IS NULL OR length(continues) = 16) REFERENCES sessions(id);
