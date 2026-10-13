-- A job's report can come from a file (D-055, jobs.rs): when an agent job's run ends `ok`, the kernel
-- posts this file (inside the job's workspace) as the run's report, instead of the final answer.
-- Expand-only: a new nullable column.
ALTER TABLE jobs ADD COLUMN report_file TEXT;
