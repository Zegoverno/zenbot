-- Models are named by their full id, never by a Claude Code alias (`opus`), which moves to a
-- new model when the CLI is updated.

-- Sessions: what each alias resolves to on Claude Code 2.1.287, the version installed with this change.
UPDATE sessions SET model = 'claude/claude-opus-5-5' WHERE model = 'claude/opus';
UPDATE sessions SET model = 'claude/claude-sonnet-5-5' WHERE model = 'claude/sonnet';
UPDATE sessions SET model = 'claude/claude-haiku-4-5-20251001' WHERE model = 'claude/haiku';

-- Recorded calls: what the alias resolved to when they ran (Claude Code 2.1.278).
UPDATE model_calls SET model = 'claude-opus-5' WHERE provider = 'claude' AND model = 'opus';
UPDATE model_calls SET model = 'claude-sonnet-5' WHERE provider = 'claude' AND model = 'sonnet';
UPDATE model_calls SET model = 'claude-haiku-4-5-20251001' WHERE provider = 'claude' AND model = 'haiku';
