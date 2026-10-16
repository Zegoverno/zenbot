-- D-067: session locks, the egress record, and the reset of 0027's legacy marks. Expand-only.
ALTER TABLE sessions ADD COLUMN lock_reason text;
ALTER TABLE sessions ADD COLUMN locked_at timestamptz;

-- Every host a session's shell reached through the egress proxy, with its rating and the decision.
CREATE TABLE egress (
    session_id uuid NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    host text NOT NULL,
    rating smallint NOT NULL,
    outcome text NOT NULL,
    count integer NOT NULL DEFAULT 1,
    first_at timestamptz NOT NULL DEFAULT now(),
    last_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (session_id, host)
);

-- 0027 rated every block of a session the old single mark had touched at 4, the owner's own
-- messages and base prompts included. Keep 4 only where outside content really entered (web and
-- MCP results); everything else of those sessions returns to 1, and each session's level is
-- recomputed from what it actually read. Contaminated sessions and owner-trusted windows are
-- left as they are. The old values stay readable in `legacy_risk`.
ALTER TABLE tape_events ADD COLUMN legacy_risk smallint;
UPDATE tape_events SET legacy_risk = risk, risk = 1
WHERE origin = 'legacy-session' AND risk = 4
  AND NOT (kind = 'message' AND payload->>'role' = 'toolResult'
           AND payload->>'toolName' IN ('web_search', 'web_fetch', 'call_tool', 'load_tool', 'find_tools'));
UPDATE sessions s SET risk = GREATEST(1, COALESCE((SELECT max(e.risk) FROM tape_events e WHERE e.session_id = s.id), 1))
WHERE s.risk_trusted_through = 0 AND NOT zen_session_contaminated(s.id)
  AND EXISTS (SELECT 1 FROM tape_events e WHERE e.session_id = s.id AND e.legacy_risk IS NOT NULL);
