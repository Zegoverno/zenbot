-- Script jobs (D-073): a job that runs a shell command in the contained shell, no model. The owner's
-- are live; one the agent creates (or changes) waits paused for the owner's approval. Each run
-- records the exact command it ran and its exit code.
ALTER TABLE jobs DROP CONSTRAINT IF EXISTS jobs_kind_check;
ALTER TABLE jobs ADD CONSTRAINT jobs_kind_check CHECK (kind IN ('agent', 'system', 'script'));
ALTER TABLE jobs ADD COLUMN command text;
ALTER TABLE job_runs ADD COLUMN command text;
ALTER TABLE job_runs ADD COLUMN exit_code integer;
