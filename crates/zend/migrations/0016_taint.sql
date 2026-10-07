-- When a session first read untrusted content (web pages, search results; web.rs). What such a
-- session saves to memory counts as inference at most (memory.rs).
ALTER TABLE sessions ADD COLUMN tainted_at TIMESTAMPTZ;
