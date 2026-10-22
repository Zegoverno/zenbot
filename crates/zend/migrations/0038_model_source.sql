-- Who chose a session's model (D-074): 'owner' (named it, or changed it with /model), 'router' (the
-- learning router picked it at the first prompt), or NULL (the default, not yet routed).
ALTER TABLE sessions ADD COLUMN model_source text;
