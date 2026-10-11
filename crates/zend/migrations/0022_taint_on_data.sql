-- Taint as a property of stored content (D-053, taint.rs): what a session that read untrusted
-- content saved carries the mark with it, so any session that later reads it gets it as untrusted
-- content (and is marked in turn). Expand-only: new columns and a table.
ALTER TABLE memories ADD COLUMN tainted BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE job_runs ADD COLUMN tainted BOOLEAN NOT NULL DEFAULT false;

-- Draft skills (`skills/_proposed/<domain>/<name>`) a session that read untrusted content wrote to,
-- by `save_skill` or `edit`/`write`. Out of the agent's reach, like `made_tools`; the row goes when
-- the owner accepts or rejects the draft.
CREATE TABLE draft_taint (
    skill       TEXT PRIMARY KEY,                    -- domain/name
    session_id  UUID,                                -- the first tainted session that wrote it
    at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- What was saved before this release: a memory written by a session already marked by then, and a
-- job run whose session was marked.
UPDATE memories m SET tainted = true FROM sessions s
 WHERE s.id = m.session_id AND s.tainted_at IS NOT NULL AND s.tainted_at <= m.updated_at;
UPDATE job_runs r SET tainted = true FROM sessions s
 WHERE s.id = r.session_id AND s.tainted_at IS NOT NULL;
