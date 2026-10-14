-- A finished job run's session is archived (D-057, jobs.rs `finish`): archive the ones from before,
-- except a run still going. Data only: no schema change, and a rollback leaves them archived, which
-- the owner can undo per session.
UPDATE sessions SET archived = true
 WHERE kind = 'job' AND NOT archived
   AND id NOT IN (SELECT session_id FROM job_runs WHERE status = 'running' AND session_id IS NOT NULL);
