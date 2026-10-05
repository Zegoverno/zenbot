-- Per-session lookups on model and tool calls (session cost in the session list, scoring).
CREATE INDEX IF NOT EXISTS model_calls_session ON model_calls(session_id);
CREATE INDEX IF NOT EXISTS tool_calls_session ON tool_calls(session_id);
