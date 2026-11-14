-- A vault task the owner handed to an agent (the Tasks view's `a`): the task's 🆔 and the session
-- working on it. The vault's task line is never written; this link lives only here.
CREATE TABLE task_agents (
    task_id    TEXT NOT NULL PRIMARY KEY,
    session_id BLOB NOT NULL CHECK (length(session_id)=16) REFERENCES sessions(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
);
