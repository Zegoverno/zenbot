-- D-062 step 2: source ratings alongside legacy marks; no old column is removed.
CREATE TABLE sources (
    source text PRIMARY KEY,
    risk smallint NOT NULL CHECK (risk IN (2,3)),
    note text NOT NULL DEFAULT '',
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE source_audit (
    id bigserial PRIMARY KEY,
    source text NOT NULL,
    action text NOT NULL,
    risk smallint,
    note text NOT NULL DEFAULT '',
    created_at timestamptz NOT NULL DEFAULT now()
);
ALTER TABLE sessions ADD COLUMN risk smallint NOT NULL DEFAULT 1 CHECK (risk BETWEEN 0 AND 5);
ALTER TABLE sessions ADD COLUMN risk_trusted_through integer NOT NULL DEFAULT 0;
UPDATE sessions SET risk = 4 WHERE risky_at IS NOT NULL OR tainted_at IS NOT NULL;
ALTER TABLE tape_events ADD COLUMN risk smallint NOT NULL DEFAULT 1 CHECK (risk BETWEEN 0 AND 5);
ALTER TABLE tape_events ADD COLUMN origin text NOT NULL DEFAULT 'legacy-internal';
-- Historical provenance cannot be recovered reliably. Mark conservatively, without rewriting hashes.
UPDATE tape_events e SET risk = s.risk, origin = 'legacy-session' FROM sessions s WHERE s.id = e.session_id;
-- Old payload roles do not authenticate an owner. Keep historical messages conservative;
-- only append_owner on the new kernel can create owner/risk0 provenance.
ALTER TABLE tool_calls ADD COLUMN risk smallint CHECK (risk BETWEEN 0 AND 5);
ALTER TABLE tool_calls ADD COLUMN origin text;
ALTER TABLE tool_calls ADD COLUMN result_sha256 text;
ALTER TABLE tool_calls ADD COLUMN source_args jsonb; -- full masked query/focus, not worker echo
ALTER TABLE tape_events ADD COLUMN tool_call_record bigint REFERENCES tool_calls(id) ON DELETE SET NULL;
CREATE TABLE source_vouches (
    session_id uuid NOT NULL REFERENCES sessions(id),
    owner_seq integer NOT NULL,
    url text NOT NULL,
    base smallint NOT NULL,
    risk smallint NOT NULL CHECK (risk BETWEEN 2 AND 4),
    choice text NOT NULL,
    PRIMARY KEY (session_id, owner_seq, url, base)
);
