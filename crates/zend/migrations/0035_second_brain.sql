-- Phase 4 (D-069): memory kinds and every writer of a short-term entry, a rebuildable index of
-- record files (knowledge, entities, long-term memory), skill-use evidence and language-aware search.
-- Expand-only: no existing row is rewritten or re-rated.
ALTER TABLE memories ADD COLUMN record_type text NOT NULL DEFAULT 'experience';

-- A short-term entry replaced by another session keeps every writer: a later deny of any of them
-- stops readers (zen_memory_contaminated), and the entry's rating never drops below theirs.
CREATE TABLE brain_memory_inputs (
 memory_id bigint NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
 writer uuid NOT NULL REFERENCES sessions(id),
 PRIMARY KEY(memory_id,writer)
);
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

-- The record index: the Markdown files are the source of truth; these rows are rebuilt from them.
-- Ratings of record files live in item_ratings, like every other file's.
CREATE TABLE brain_records (
 id text PRIMARY KEY, store text NOT NULL, kind text NOT NULL, title text NOT NULL,
 path text NOT NULL UNIQUE, digest text NOT NULL, modified timestamptz NOT NULL,
 metadata jsonb NOT NULL DEFAULT '{}'
);
CREATE TABLE brain_aliases (alias text NOT NULL, record_id text NOT NULL, PRIMARY KEY(alias,record_id));
CREATE INDEX brain_aliases_record ON brain_aliases(record_id);
CREATE TABLE brain_links (source text NOT NULL, relation text NOT NULL, target text NOT NULL, PRIMARY KEY(source,relation,target));
CREATE INDEX brain_links_target ON brain_links(target);

-- Which skill version each session loaded; the owner's verdict on the session is attached later.
CREATE TABLE brain_skill_uses (
 id bigserial PRIMARY KEY, session_id uuid NOT NULL REFERENCES sessions(id),
 name text NOT NULL, digest text NOT NULL, outcome text, at timestamptz NOT NULL DEFAULT now()
);

-- English and Portuguese stemming beside the identifier-preserving `simple` index: one table
-- rewrite for both columns.
ALTER TABLE search_docs
 ADD COLUMN fts_en tsvector GENERATED ALWAYS AS (to_tsvector('english',coalesce(title,'')||' '||body)) STORED,
 ADD COLUMN fts_pt tsvector GENERATED ALWAYS AS (to_tsvector('portuguese',coalesce(title,'')||' '||body)) STORED;
CREATE INDEX search_docs_fts_en ON search_docs USING gin(fts_en);
CREATE INDEX search_docs_fts_pt ON search_docs USING gin(fts_pt);
