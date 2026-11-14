-- The security model, the learning router and skill drafts are gone: drop what only they used.
-- tape_events.tool_call_record stays (SQLite can't drop a foreign-key column without rebuilding
-- the tape); nothing reads it.

DROP VIEW zen_contaminated_memories;
DROP VIEW zen_contaminated_sessions;
DROP TRIGGER memories_origin_insert;
DROP TRIGGER memories_origin_update;
DROP TRIGGER job_runs_origin_insert;
DROP TRIGGER job_runs_origin_update;

DROP TABLE policies;
DROP TABLE made_tools;
DROP TABLE draft_taint;
DROP TABLE action_gates;
DROP TABLE source_audit;
DROP TABLE source_vouches;
DROP TABLE source_ratings;
DROP TABLE source_proposals;
DROP TABLE source_rerates;
DROP TABLE sources;
DROP TABLE item_ratings;
DROP TABLE contamination;
DROP TABLE session_inputs;
DROP TABLE egress;
DROP TABLE brain_memory_inputs;
DROP TABLE s1_calibration;
DROP TABLE s1_proposals;

ALTER TABLE sessions DROP COLUMN tainted_at;
ALTER TABLE sessions DROP COLUMN risky_at;
ALTER TABLE sessions DROP COLUMN risk;
ALTER TABLE sessions DROP COLUMN risk_trusted_through;
ALTER TABLE sessions DROP COLUMN lock_reason;
ALTER TABLE sessions DROP COLUMN locked_at;

ALTER TABLE tape_events DROP COLUMN risk;
ALTER TABLE tape_events DROP COLUMN origin;
ALTER TABLE tape_events DROP COLUMN tainted;
ALTER TABLE tape_events DROP COLUMN legacy_risk;

ALTER TABLE tool_calls DROP COLUMN risk;
ALTER TABLE tool_calls DROP COLUMN origin;
ALTER TABLE tool_calls DROP COLUMN source_args;

ALTER TABLE memories DROP COLUMN tainted;
ALTER TABLE memories DROP COLUMN risk;
ALTER TABLE memories DROP COLUMN rating_reason;
ALTER TABLE memories DROP COLUMN rating_writer_risk;

ALTER TABLE job_runs DROP COLUMN tainted;
ALTER TABLE job_runs DROP COLUMN risk;
ALTER TABLE job_runs DROP COLUMN rating_reason;
ALTER TABLE job_runs DROP COLUMN rating_writer_risk;

ALTER TABLE jobs DROP COLUMN approval;
ALTER TABLE jobs DROP COLUMN owner_approved;

-- The learning router's nightly job. Its runs stay; the job no longer runs.
UPDATE jobs SET enabled = false, removed_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
 WHERE kind = 'system' AND action = 'routes' AND removed_at IS NULL;
