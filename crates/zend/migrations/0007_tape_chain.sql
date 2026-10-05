-- The tape as a chain of blocks (docs/context.md). `seq` is a block's number within its session
-- (its address, e.g. #12), `parent` the previous block, `hash` = sha256 of the parent's hash, the
-- kind and the payload, so the last block's hash fingerprints the session up to it.
ALTER TABLE tape_events
    ADD COLUMN seq    INT,
    ADD COLUMN parent BIGINT REFERENCES tape_events(id),
    ADD COLUMN hash   TEXT;

CREATE FUNCTION zen_block_hash(parent_hash TEXT, kind TEXT, payload JSONB) RETURNS TEXT
    LANGUAGE sql IMMUTABLE
    AS $$ SELECT encode(sha256(convert_to(COALESCE(parent_hash, '') || kind || payload::text, 'UTF8')), 'hex') $$;

-- Number, link and hash a session's blocks in the order they were written. Also repairs blocks
-- that a rolled-back (older) build appended without a number.
CREATE FUNCTION zen_rechain(sid UUID) RETURNS void
    LANGUAGE plpgsql
    AS $$
DECLARE
    r RECORD;
    n INT := 0;
    prev_id BIGINT := NULL;
    prev_hash TEXT := NULL;
BEGIN
    UPDATE tape_events SET seq = NULL WHERE session_id = sid;
    FOR r IN SELECT id, kind, payload FROM tape_events WHERE session_id = sid ORDER BY id LOOP
        n := n + 1;
        prev_hash := zen_block_hash(prev_hash, r.kind, r.payload);
        UPDATE tape_events SET seq = n, parent = prev_id, hash = prev_hash WHERE id = r.id;
        prev_id := r.id;
    END LOOP;
END $$;

SELECT zen_rechain(id) FROM sessions;
CREATE UNIQUE INDEX tape_events_seq ON tape_events(session_id, seq);
