-- Plugins (plugins.rs, ADR 0001): what is installed in ~/.zenbot/plugins/<name>/, the permissions
-- the owner approved (never more than its manifest asked for), and whether its server runs.
CREATE TABLE plugins (
 name TEXT NOT NULL PRIMARY KEY,
 version TEXT NOT NULL,
 manifest TEXT NOT NULL CHECK (json_valid(manifest)),
 permissions TEXT NOT NULL CHECK (json_valid(permissions)),
 enabled INTEGER NOT NULL DEFAULT 1,
 -- marketplace, archive or folder: where the installed copy came from.
 source TEXT NOT NULL,
 installed_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
 updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
);
-- The owner's default per slot: key `<pattern>:<mode>` (an opener), value `<plugin>/<opener id>`
-- or `builtin`. An install never writes here.
CREATE TABLE plugin_defaults (
 key TEXT NOT NULL PRIMARY KEY,
 value TEXT NOT NULL,
 updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
);
-- Calls that reach a plugin's server from outside a turn: a UI's tool call (`ui`, also in
-- tool_calls when it names a session) and the server's own sampling requests (`sampling`).
CREATE TABLE plugin_calls (
 id INTEGER PRIMARY KEY AUTOINCREMENT,
 plugin TEXT NOT NULL,
 caller TEXT NOT NULL CHECK (caller IN ('ui', 'sampling')),
 tool TEXT,
 session_id BLOB CHECK (session_id IS NULL OR length(session_id)=16),
 is_error INTEGER NOT NULL,
 duration_ms INTEGER NOT NULL,
 output_bytes INTEGER NOT NULL,
 model TEXT,
 input_tokens INTEGER,
 output_tokens INTEGER,
 result_sha256 TEXT,
 created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
);
CREATE INDEX plugin_calls_plugin ON plugin_calls (plugin, created_at);
