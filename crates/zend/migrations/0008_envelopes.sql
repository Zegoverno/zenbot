-- A session's instructions (system prompt) and tools, stored once per distinct pair and fixed for
-- the session (docs/context.md). The tape records which one a session uses (`envelope` blocks).
CREATE TABLE envelopes (
    hash        TEXT PRIMARY KEY,           -- sha256 of the system prompt and the tools
    system      TEXT NOT NULL,
    tools       JSONB NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
