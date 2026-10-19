-- Durable owner prompts, consumed atomically with their owner tape message.
CREATE TABLE pending_prompts (
 id bigserial PRIMARY KEY,
 session_id uuid NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
 text text NOT NULL,
 steer boolean NOT NULL DEFAULT false,
 created_at timestamptz NOT NULL DEFAULT now(),
 error text
);
CREATE INDEX pending_prompts_session ON pending_prompts(session_id,id) WHERE error IS NULL;

-- Explicit owner approval is reset whenever an agent edits a job definition.
ALTER TABLE jobs ADD COLUMN owner_approved boolean NOT NULL DEFAULT false;
UPDATE jobs SET owner_approved=true WHERE kind='agent' AND (created_by='owner' OR approval IS NULL);

-- A committed dirty queue makes JSONL exports retryable without assuming id/commit ordering.
CREATE TABLE tape_exports (
 session_id uuid PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
 version bigint NOT NULL DEFAULT 1
);
CREATE FUNCTION zen_dirty_export() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 INSERT INTO tape_exports(session_id) VALUES(NEW.session_id)
 ON CONFLICT(session_id) DO UPDATE SET version=tape_exports.version+1;
 RETURN NEW;
END $$;
CREATE TRIGGER tape_export_dirty AFTER INSERT OR UPDATE ON tape_events
 FOR EACH ROW EXECUTE FUNCTION zen_dirty_export();
INSERT INTO tape_exports(session_id) SELECT DISTINCT session_id FROM tape_events;

-- Source-rating policy v2 needs fresh metadata judgments; explicit owner entries stay intact.
UPDATE source_ratings SET rated_at=now()-interval '31 days';
