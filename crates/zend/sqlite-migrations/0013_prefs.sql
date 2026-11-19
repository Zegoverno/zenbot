-- The owner's preferences from the web app (prefs.rs): a key and its JSON value. One row per
-- preference: `name`, `home_layout`, `default_model`, `default_effort`.
CREATE TABLE prefs (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL CHECK (json_valid(value)),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
);
