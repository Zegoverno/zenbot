ALTER TABLE memories ADD COLUMN details jsonb NOT NULL DEFAULT '{}';
-- Phase 4: three curated stores, explicit evidence, revision-safe operations and recoverable work.
ALTER TABLE memories ADD COLUMN pinned boolean NOT NULL DEFAULT false;
ALTER TABLE memories ADD COLUMN record_type text NOT NULL DEFAULT 'experience';
ALTER TABLE memories ADD COLUMN evidence jsonb NOT NULL DEFAULT '[]';
ALTER TABLE memories ADD COLUMN event_at timestamptz;
ALTER TABLE memories ADD COLUMN entities jsonb NOT NULL DEFAULT '[]';
ALTER TABLE memories ADD COLUMN revision bigint NOT NULL DEFAULT 1;
ALTER TABLE memories ADD COLUMN capture_key text;
CREATE UNIQUE INDEX memories_capture_key ON memories(capture_key) WHERE capture_key IS NOT NULL;

CREATE TABLE brain_revisions (
 id bigserial PRIMARY KEY, record_id text NOT NULL, revision bigint NOT NULL,
 snapshot jsonb NOT NULL, at timestamptz NOT NULL DEFAULT now(), UNIQUE(record_id,revision)
);
CREATE FUNCTION zen_memory_revision() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP='UPDATE' AND (NEW.text,NEW.source,NEW.tier,NEW.session_id,NEW.pinned,NEW.record_type,NEW.evidence,NEW.entities,NEW.event_at,NEW.details) IS DISTINCT FROM (OLD.text,OLD.source,OLD.tier,OLD.session_id,OLD.pinned,OLD.record_type,OLD.evidence,OLD.entities,OLD.event_at,OLD.details) THEN
   INSERT INTO brain_revisions(record_id,revision,snapshot) VALUES('m'||OLD.id,OLD.revision,to_jsonb(OLD)) ON CONFLICT DO NOTHING;
   NEW.revision := OLD.revision+1;
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER memories_revision BEFORE UPDATE ON memories FOR EACH ROW EXECUTE FUNCTION zen_memory_revision();

CREATE TABLE brain_records (
 id text PRIMARY KEY, store text NOT NULL CHECK(store IN ('knowledge','entity')),
 kind text NOT NULL, title text NOT NULL, body text NOT NULL,
 metadata jsonb NOT NULL DEFAULT '{}', revision bigint NOT NULL DEFAULT 1,
 session_id uuid REFERENCES sessions(id), risk smallint NOT NULL DEFAULT 4 CHECK(risk BETWEEN 0 AND 5),
 status text NOT NULL DEFAULT 'active', path text NOT NULL UNIQUE, digest text,
 published boolean NOT NULL DEFAULT false, updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE brain_aliases (
 alias text NOT NULL, record_id text NOT NULL REFERENCES brain_records(id),
 PRIMARY KEY(alias,record_id)
);
CREATE TABLE brain_links (
 source text NOT NULL REFERENCES brain_records(id), source_anchor text NOT NULL DEFAULT '', relation text NOT NULL,
 target text NOT NULL REFERENCES brain_records(id), target_anchor text NOT NULL DEFAULT '', evidence jsonb NOT NULL DEFAULT '[]',
 tentative boolean NOT NULL DEFAULT false, PRIMARY KEY(source,source_anchor,relation,target,target_anchor)
);
CREATE INDEX brain_links_target ON brain_links(target);
CREATE TABLE brain_work (
 id bigserial PRIMARY KEY, kind text NOT NULL, work_key text NOT NULL UNIQUE,
 payload jsonb NOT NULL, state text NOT NULL DEFAULT 'pending', attempts int NOT NULL DEFAULT 0,
 lease_until timestamptz, error text, result jsonb, updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX brain_work_pending ON brain_work(state,lease_until);
CREATE TABLE brain_contexts (
 id uuid PRIMARY KEY, session_id uuid NOT NULL REFERENCES sessions(id), query text NOT NULL,
 receipt jsonb NOT NULL, at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX brain_contexts_session ON brain_contexts(session_id,at DESC);
CREATE TABLE brain_skill_uses (
 id bigserial PRIMARY KEY, session_id uuid NOT NULL REFERENCES sessions(id),
 name text NOT NULL, digest text NOT NULL, outcome text, at timestamptz NOT NULL DEFAULT now()
);

-- Migration manifest and first export are durable; no reclassification or risk lowering.
INSERT INTO brain_work(kind,work_key,payload)
 SELECT 'memory_export','phase4-initial-export',jsonb_build_object('legacy_count',count(*),'max_id',max(id)) FROM memories;

-- Add language-aware arms without replacing the identifier-preserving simple index.
ALTER TABLE search_docs ADD COLUMN fts_en tsvector GENERATED ALWAYS AS (to_tsvector('english',coalesce(title,'')||' '||body)) STORED;
ALTER TABLE search_docs ADD COLUMN fts_pt tsvector GENERATED ALWAYS AS (to_tsvector('portuguese',coalesce(title,'')||' '||body)) STORED;
CREATE INDEX search_docs_fts_en ON search_docs USING gin(fts_en);
CREATE INDEX search_docs_fts_pt ON search_docs USING gin(fts_pt);

CREATE TABLE brain_memory_inputs (memory_id bigint NOT NULL REFERENCES memories(id) ON DELETE CASCADE,writer uuid NOT NULL REFERENCES sessions(id),PRIMARY KEY(memory_id,writer));
INSERT INTO brain_memory_inputs SELECT id,session_id FROM memories WHERE session_id IS NOT NULL;
CREATE FUNCTION zen_memory_inputs() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF NEW.session_id IS NOT NULL THEN INSERT INTO brain_memory_inputs VALUES(NEW.id,NEW.session_id) ON CONFLICT DO NOTHING; END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER memory_inputs AFTER INSERT OR UPDATE OF session_id ON memories FOR EACH ROW EXECUTE FUNCTION zen_memory_inputs();
CREATE FUNCTION zen_memory_contaminated(target bigint) RETURNS boolean LANGUAGE sql STABLE AS $$
 SELECT EXISTS(SELECT 1 FROM brain_memory_inputs WHERE memory_id=target AND zen_session_contaminated(writer))
$$;

CREATE TABLE brain_record_inputs (record_id text NOT NULL REFERENCES brain_records(id),writer uuid NOT NULL REFERENCES sessions(id),PRIMARY KEY(record_id,writer));
CREATE FUNCTION zen_brain_contaminated(target text) RETURNS boolean LANGUAGE sql STABLE AS $$
 SELECT EXISTS(SELECT 1 FROM brain_record_inputs WHERE record_id=target AND zen_session_contaminated(writer))
$$;
