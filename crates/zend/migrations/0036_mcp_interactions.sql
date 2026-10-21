-- Owner responses to MCP form elicitation, correlated with an active originating call.
CREATE TABLE mcp_interactions (
 id uuid PRIMARY KEY,
 session_id uuid NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
 server text NOT NULL,
 request_id jsonb NOT NULL,
 message text NOT NULL,
 schema jsonb NOT NULL,
 status text NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','accept','decline','cancel')),
 content jsonb,
 created_at timestamptz NOT NULL DEFAULT now(),
 expires_at timestamptz NOT NULL DEFAULT now() + interval '5 minutes'
);
CREATE INDEX mcp_interactions_pending ON mcp_interactions(session_id) WHERE status='pending';
