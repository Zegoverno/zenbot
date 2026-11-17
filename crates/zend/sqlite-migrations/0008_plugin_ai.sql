-- Plugin AI (plugins.rs `sample`): the owner's per-plugin switch for a plugin granted `ai` (on by
-- default; off refuses its sampling requests without stopping its server), and each sampling
-- call's cost as the engine reported it.
ALTER TABLE plugins ADD COLUMN ai_enabled INTEGER NOT NULL DEFAULT 1;
ALTER TABLE plugin_calls ADD COLUMN cost_usd REAL;
