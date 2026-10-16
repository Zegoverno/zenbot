-- D-065: metadata-only source judgments, owner-reviewed immutable proposal snapshots.
CREATE TABLE source_ratings (
 scope text PRIMARY KEY,
 risk smallint NOT NULL CHECK (risk BETWEEN 2 AND 4),
 answers jsonb NOT NULL,
 model text NOT NULL,
 rated_at timestamptz NOT NULL DEFAULT now(),
 proposal text CHECK (proposal IN ('pending','approved','rejected')),
 proposed_at timestamptz,
 snapshot uuid NOT NULL
);
CREATE TABLE source_proposals (
 id bigserial PRIMARY KEY,
 scope text NOT NULL,
 risk smallint NOT NULL CHECK (risk=2),
 snapshot uuid NOT NULL UNIQUE,
 created_at timestamptz NOT NULL DEFAULT now(),
 decision text CHECK (decision IN ('approved','rejected')),
 decided_at timestamptz
);
-- Do not backfill evidence: old rows' reasons cannot be reconstructed.
ALTER TABLE memories ADD COLUMN rating_reason text NOT NULL DEFAULT 'unknown';
ALTER TABLE memories ADD COLUMN rating_writer_risk smallint;
ALTER TABLE job_runs ADD COLUMN rating_reason text NOT NULL DEFAULT 'unknown';
ALTER TABLE job_runs ADD COLUMN rating_writer_risk smallint;
CREATE FUNCTION zen_source_origin_evidence() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE r smallint;
BEGIN
    SELECT risk INTO r FROM sessions WHERE id=NEW.session_id FOR SHARE;
    NEW.rating_writer_risk := r;
    NEW.rating_reason := CASE WHEN NEW.risk=0 THEN 'owner'
        WHEN r IS NOT NULL AND NEW.risk=r THEN 'writer_session' ELSE 'unknown' END;
    RETURN NEW;
END $$;
-- Trigger names sort after the existing origin trigger, which authenticates NEW.risk.
CREATE TRIGGER zz_memories_rating_evidence BEFORE INSERT OR UPDATE OF text,session_id ON memories
 FOR EACH ROW EXECUTE FUNCTION zen_source_origin_evidence();
CREATE TRIGGER zz_job_runs_rating_evidence BEFORE INSERT OR UPDATE OF output,session_id ON job_runs
 FOR EACH ROW EXECUTE FUNCTION zen_source_origin_evidence();

CREATE TABLE source_rerates (
 version integer PRIMARY KEY,
 completed_at timestamptz NOT NULL DEFAULT now(),
 note text NOT NULL
);
