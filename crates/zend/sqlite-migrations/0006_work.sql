-- Work projections of work_created/work_submitted/work_interrupted tape events.
-- Contracts are immutable; execution completion is never verification or acceptance.
CREATE TABLE work_items (
 id BLOB NOT NULL PRIMARY KEY CHECK(length(id)=16),
 session_id BLOB NOT NULL REFERENCES sessions(id),
 agent TEXT NOT NULL,
 request_key TEXT NOT NULL,
 request TEXT NOT NULL CHECK(json_valid(request)),
 contract TEXT NOT NULL CHECK(json_valid(contract)),
 vault_task_id TEXT,
 created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
 UNIQUE(session_id,request_key)
);
CREATE TABLE work_attempts (
 id BLOB NOT NULL PRIMARY KEY CHECK(length(id)=16),
 work_id BLOB NOT NULL REFERENCES work_items(id),
 session_id BLOB NOT NULL REFERENCES sessions(id),
 start_seq INTEGER NOT NULL CHECK(start_seq>0),
 status TEXT NOT NULL CHECK(status IN ('running','completed','interrupted')),
 started_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
 ended_at TEXT,
 submission_key TEXT,
 submission TEXT CHECK(submission IS NULL OR json_valid(submission)),
 CHECK ((status='running' AND ended_at IS NULL) OR (status<>'running' AND ended_at IS NOT NULL))
);
CREATE INDEX work_attempts_work ON work_attempts(work_id);
CREATE TABLE work_artifacts (
 id BLOB NOT NULL PRIMARY KEY CHECK(length(id)=16),
 attempt_id BLOB NOT NULL REFERENCES work_attempts(id),
 kind TEXT NOT NULL CHECK(kind IN ('answer','file')),
 reference TEXT NOT NULL CHECK(json_valid(reference)),
 sha256 TEXT NOT NULL CHECK(length(sha256)=64)
);
CREATE INDEX work_artifacts_attempt ON work_artifacts(attempt_id);
