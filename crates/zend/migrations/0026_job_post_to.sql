-- A job's report can go into a chosen session (D-060, jobs.rs): when an agent job's run ends `ok`,
-- the kernel appends the report there as an assistant message, and the Matrix bridge posts it in that
-- session's room instead of "zen jobs". `job_runs.posted_to` records where a run's report went.
-- Expand-only: new nullable columns.
ALTER TABLE jobs ADD COLUMN post_to UUID;
ALTER TABLE job_runs ADD COLUMN posted_to UUID;
-- Durable delivery intent: set when the run finishes, cleared atomically with the tape append.
ALTER TABLE job_runs ADD COLUMN pending_to UUID;
