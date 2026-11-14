-- Private agents (agents.rs): whose each short-term memory and each search document is. Every
-- existing one is zenbot's. A record's owner is the vault its file sits in (brain_records.path).
ALTER TABLE memories ADD COLUMN agent TEXT NOT NULL DEFAULT 'zenbot';
ALTER TABLE search_docs ADD COLUMN agent TEXT NOT NULL DEFAULT 'zenbot';
CREATE INDEX memories_agent_tier ON memories (agent, tier);
-- A session turn is its session's agent's (search scopes sessions by agent).
UPDATE search_docs SET agent = coalesce((SELECT s.agent FROM sessions s WHERE s.id = search_docs.session_id), 'zenbot')
WHERE kind = 'turn' AND session_id IS NOT NULL;
