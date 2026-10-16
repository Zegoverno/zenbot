-- D-062 derived content retains writer dependencies, not only a numeric risk.
-- This separate expand-only migration follows the draft migration tested on throwaway DBs.
CREATE TABLE session_inputs (
    reader uuid NOT NULL REFERENCES sessions(id),
    writer uuid NOT NULL REFERENCES sessions(id),
    PRIMARY KEY(reader,writer),
    CHECK(reader != writer)
);
CREATE INDEX session_inputs_writer ON session_inputs(writer);
CREATE INDEX item_ratings_digest ON item_ratings(sha256);

CREATE OR REPLACE FUNCTION zen_session_contaminated(target uuid) RETURNS boolean
LANGUAGE sql STABLE AS $$
 WITH RECURSIVE edges AS (
   SELECT id AS reader,parent AS writer FROM sessions WHERE parent IS NOT NULL
   UNION SELECT reader,writer FROM session_inputs
 ), ancestry(id) AS (
   SELECT target
   UNION SELECT e.writer FROM edges e JOIN ancestry a ON a.id=e.reader
 )
 SELECT EXISTS(SELECT 1 FROM contamination c JOIN ancestry a ON a.id=c.affected
               WHERE c.cleared_at IS NULL)
$$;
