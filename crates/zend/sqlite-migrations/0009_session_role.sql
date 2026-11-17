-- The role a subagent session was given (roles.rs, `delegate` with `role`), so its outcomes can be
-- grouped by role. NULL for every other session.
ALTER TABLE sessions ADD COLUMN role TEXT;
