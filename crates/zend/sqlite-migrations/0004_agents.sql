-- Agents (agents.rs): who each session is, and who each agent job runs as. Every existing session
-- and job is zenbot's.
ALTER TABLE sessions ADD COLUMN agent TEXT NOT NULL DEFAULT 'zenbot';
ALTER TABLE jobs ADD COLUMN agent TEXT NOT NULL DEFAULT 'zenbot';
