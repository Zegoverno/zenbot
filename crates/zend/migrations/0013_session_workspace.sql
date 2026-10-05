-- A session's own working directory for tools, when it isn't the kernel's workspace (e.g. a
-- verifier runs in the repository its brief names). NULL: the kernel's workspace.
ALTER TABLE sessions ADD COLUMN workspace TEXT;
