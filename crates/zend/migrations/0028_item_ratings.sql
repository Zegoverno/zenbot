-- D-062 step 3: immutable origin risk is separate from reversible contamination.
-- Legacy marks are risk 4, not evidence of an attack. Old columns remain for rollback.
ALTER TABLE memories ADD COLUMN risk smallint NOT NULL DEFAULT 4 CHECK (risk BETWEEN 0 AND 5);
UPDATE memories m SET risk = CASE WHEN m.tainted THEN 4 ELSE COALESCE(s.risk,4) END
 FROM sessions s WHERE s.id=m.session_id;
ALTER TABLE job_runs ADD COLUMN risk smallint NOT NULL DEFAULT 4 CHECK (risk BETWEEN 0 AND 5);
UPDATE job_runs r SET risk = CASE WHEN r.tainted THEN 4 ELSE COALESCE(s.risk,4) END
 FROM sessions s WHERE s.id=r.session_id;

-- Versioned file provenance: editing a file cannot reuse the trust of an older digest.
CREATE TABLE item_ratings (
    id bigserial PRIMARY KEY,
    path text NOT NULL,
    sha256 text NOT NULL,
    session_id uuid REFERENCES sessions(id),
    risk smallint NOT NULL CHECK (risk BETWEEN 0 AND 5),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE(path,sha256,session_id)
);
CREATE INDEX item_ratings_path ON item_ratings(path,sha256);

-- Multiple independent causes may contaminate the same session. Trust clears only causes
-- included in the owner's review, never a boolean shared by unrelated writers.
CREATE TABLE contamination (
    cause uuid NOT NULL REFERENCES sessions(id),
    affected uuid NOT NULL REFERENCES sessions(id),
    at timestamptz NOT NULL DEFAULT now(),
    cleared_at timestamptz,
    PRIMARY KEY(cause,affected)
);
CREATE INDEX contamination_affected ON contamination(affected) WHERE cleared_at IS NULL;
ALTER TABLE tape_events ADD COLUMN tainted boolean NOT NULL DEFAULT false;

CREATE FUNCTION zen_session_contaminated(target uuid) RETURNS boolean
LANGUAGE sql STABLE AS $$
 WITH RECURSIVE ancestry AS (
   SELECT id,parent FROM sessions WHERE id=target
   UNION SELECT s.id,s.parent FROM sessions s JOIN ancestry a ON s.id=a.parent
 )
 SELECT EXISTS(SELECT 1 FROM contamination c JOIN ancestry a ON a.id=c.affected
               WHERE c.cleared_at IS NULL)
$$;

-- Record a stored row's origin in the same transaction that changes its contents.
-- A caller cannot claim a lower rating by supplying a risk field.
CREATE FUNCTION zen_item_origin() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE r smallint;
BEGIN
    IF NEW.session_id IS NULL THEN
        NEW.risk := 4;
    ELSE
        SELECT risk INTO r FROM sessions WHERE id=NEW.session_id FOR SHARE;
        NEW.risk := COALESCE(r,4);
        -- Whole authenticated owner turn only; never a substring or model source assertion.
        IF TG_TABLE_NAME = 'memories' AND EXISTS (
            SELECT 1 FROM tape_events WHERE session_id=NEW.session_id
            AND origin='owner' AND risk=0 AND kind='message'
            AND payload->>'role'='user'
            AND (payload->>'content') = to_jsonb(NEW)->>'text'
        ) THEN
            NEW.risk := 0;
        END IF;
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER memories_origin BEFORE INSERT OR UPDATE OF text,session_id ON memories
 FOR EACH ROW EXECUTE FUNCTION zen_item_origin();
CREATE TRIGGER job_runs_origin BEFORE INSERT OR UPDATE OF output,session_id ON job_runs
 FOR EACH ROW EXECUTE FUNCTION zen_item_origin();
