-- SQLite baseline of PostgreSQL migrations 0001–0038. Historical migrations remain immutable.
-- UUIDs use SQLx native 16-byte BLOB CHECK (byte IS NULL OR length(byte)=16)s; arrays/JSON use JSON text; vectors use float32 BLOB CHECK (float32 IS NULL OR length(float32)=16)s.

CREATE TABLE sessions (
id          BLOB CHECK (id IS NULL OR length(id)=16) NOT NULL PRIMARY KEY,
    title       TEXT NOT NULL DEFAULT '',
    model       TEXT NOT NULL,
    archived    INTEGER NOT NULL DEFAULT FALSE,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
    updated_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
effort TEXT,
state  TEXT,
parent BLOB CHECK (parent IS NULL OR length(parent)=16) REFERENCES sessions(id),
kind   TEXT,
workspace TEXT,
tainted_at TEXT,
title_source TEXT NOT NULL DEFAULT 'auto',
risky_at TEXT,
risk INTEGER NOT NULL DEFAULT 1 CHECK (risk BETWEEN 0 AND 5),
risk_trusted_through integer NOT NULL DEFAULT 0,
lock_reason text,
locked_at TEXT,
model_source text
);

CREATE TABLE tape_events (
id          INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id  BLOB CHECK (session_id IS NULL OR length(session_id)=16) NOT NULL REFERENCES sessions(id),
    kind        TEXT NOT NULL,
    payload     TEXT NOT NULL,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
seq    INTEGER,
parent INTEGER REFERENCES tape_events(id),
hash   TEXT,
risk INTEGER NOT NULL DEFAULT 1 CHECK (risk BETWEEN 0 AND 5),
origin text NOT NULL DEFAULT 'legacy-internal',
tool_call_record INTEGER REFERENCES tool_calls(id) ON DELETE SET NULL,
tainted INTEGER NOT NULL DEFAULT false,
legacy_risk INTEGER,
hash_payload TEXT,
hash_version INTEGER NOT NULL DEFAULT 2 CHECK(hash_version IN (1,2))
);

CREATE TABLE model_calls (
id            INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id    BLOB CHECK (session_id IS NULL OR length(session_id)=16) NOT NULL REFERENCES sessions(id),
    provider      TEXT NOT NULL,
    model         TEXT NOT NULL,
    input_tokens  INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    cache_read    INTEGER NOT NULL,
    cache_write   INTEGER NOT NULL,
    cost_usd      REAL NOT NULL,
    stop_reason   TEXT,
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
turn_id BLOB CHECK (turn_id IS NULL OR length(turn_id)=16) REFERENCES turns(id),
duration_ms INTEGER
);

CREATE TABLE tool_calls (
id           INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id   BLOB CHECK (session_id IS NULL OR length(session_id)=16) NOT NULL REFERENCES sessions(id),
    call_id      TEXT NOT NULL,
    name         TEXT NOT NULL,
    args         TEXT NOT NULL,
    is_error     INTEGER NOT NULL,
    duration_ms  INTEGER NOT NULL,
    output_bytes INTEGER NOT NULL,
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
turn_id BLOB CHECK (turn_id IS NULL OR length(turn_id)=16) REFERENCES turns(id),
risk INTEGER CHECK (risk BETWEEN 0 AND 5),
origin text,
result_sha256 text,
source_args TEXT
);

CREATE TABLE turns (
id              BLOB CHECK (id IS NULL OR length(id)=16) NOT NULL PRIMARY KEY,
    session_id      BLOB CHECK (session_id IS NULL OR length(session_id)=16) NOT NULL REFERENCES sessions(id),
    harness         TEXT NOT NULL,
    worker          TEXT NOT NULL,
    engine          TEXT,
    engine_version  TEXT,
    model           TEXT NOT NULL,
    model_resolved  TEXT,
    effort          TEXT,
    started_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
    ended_at        TEXT,
    duration_ms     INTEGER,
    outcome         TEXT,
    error           TEXT,
    model_calls     INTEGER,
    tool_calls      INTEGER,
    tool_errors     INTEGER,
    input_tokens    INTEGER,
    output_tokens   INTEGER,
    cache_read      INTEGER,
    cache_write     INTEGER,
    cost_usd        REAL,
    usage           TEXT,
envelope        TEXT,
context         TEXT,
context_tokens  INTEGER,
cache_break     TEXT
);

CREATE TABLE session_decisions (
id          INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id  BLOB CHECK (session_id IS NULL OR length(session_id)=16) NOT NULL REFERENCES sessions(id),
    turn_id     BLOB CHECK (turn_id IS NULL OR length(turn_id)=16) REFERENCES turns(id),
    decision    TEXT NOT NULL CHECK (decision IN ('accept', 'more', 'reshape', 'drop')),
    note        TEXT,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
source TEXT NOT NULL DEFAULT 'owner'
);

CREATE TABLE session_scores (
id               INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id       BLOB CHECK (session_id IS NULL OR length(session_id)=16) NOT NULL REFERENCES sessions(id),
    turn_id          BLOB CHECK (turn_id IS NULL OR length(turn_id)=16) REFERENCES turns(id),
    scorer           TEXT NOT NULL,
    scorer_resolved  TEXT,
    questions        TEXT NOT NULL,
    trigger          TEXT NOT NULL,
    answers          TEXT,
    usage            TEXT,
    cost_usd         REAL,
    error            TEXT,
    created_at       TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
);

CREATE TABLE envelopes (
hash        TEXT NOT NULL PRIMARY KEY,
    system      TEXT NOT NULL,
    tools       TEXT NOT NULL,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
);

CREATE TABLE compactions (
id           INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id   BLOB CHECK (session_id IS NULL OR length(session_id)=16) NOT NULL REFERENCES sessions(id),
    covers_from  INTEGER NOT NULL,
    covers_to    INTEGER NOT NULL,
    text         TEXT NOT NULL,
    data         TEXT,
    model        TEXT NOT NULL,
    usage        TEXT,
    cost_usd     REAL,
    error        TEXT,
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
    applied_seq  INTEGER
);

CREATE TABLE decisions (
id           INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id   BLOB CHECK (session_id IS NULL OR length(session_id)=16) REFERENCES sessions(id),
    point        TEXT NOT NULL,
    model        TEXT,
    input        TEXT,
    answer       TEXT,
    chosen       TEXT,
    probability  REAL,
    acted        INTEGER NOT NULL DEFAULT false,
    actual       TEXT,
    actual_by    TEXT,
    error        TEXT,
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
    resolved_at  TEXT,
spec_sha text,
spec_version integer,
rung integer,
latency_ms integer,
cost_usd REAL,
tokens INTEGER,
source text,
items integer
);

CREATE TABLE policies (
version     INTEGER PRIMARY KEY AUTOINCREMENT,
    data        TEXT NOT NULL,
    reason      TEXT,
    created_by  TEXT NOT NULL,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
);

CREATE TABLE memories (
id          INTEGER PRIMARY KEY AUTOINCREMENT,
    text        TEXT NOT NULL,
    source      TEXT NOT NULL DEFAULT 'inferred',
    tier        TEXT NOT NULL DEFAULT 'short',
    session_id  BLOB CHECK (session_id IS NULL OR length(session_id)=16) REFERENCES sessions(id),
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
    updated_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
    used_at     TEXT,
    uses        INTEGER NOT NULL DEFAULT 0,
    scores      TEXT,
    proposed    TEXT,
    reason      TEXT,
    supersedes  INTEGER REFERENCES memories(id),
tainted INTEGER NOT NULL DEFAULT false,
risk INTEGER NOT NULL DEFAULT 4 CHECK (risk BETWEEN 0 AND 5),
rating_reason text NOT NULL DEFAULT 'unknown',
rating_writer_risk INTEGER,
record_type text NOT NULL DEFAULT 'experience'
);

CREATE TABLE sleep_runs (
id          INTEGER PRIMARY KEY AUTOINCREMENT,
    started_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
    ended_at    TEXT,
    trigger     TEXT NOT NULL,
    scorer      TEXT,
    entries     INTEGER,
    kept        INTEGER,
    dropped     INTEGER,
    promoted    INTEGER,
    proposed    INTEGER,
    note        TEXT,
    error       TEXT
);

CREATE TABLE search_docs (
id          INTEGER PRIMARY KEY AUTOINCREMENT,
    kind        TEXT NOT NULL,
    ref         TEXT NOT NULL,
    session_id  BLOB CHECK (session_id IS NULL OR length(session_id)=16) REFERENCES sessions(id),
    ident       TEXT,
    title       TEXT,
    body        TEXT NOT NULL,

    embedding   BLOB CHECK (embedding IS NULL OR length(embedding)=6144),
    embed_model TEXT,
    at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
    updated_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
embed_attempts INTEGER NOT NULL DEFAULT 0,
embed_error TEXT,
    UNIQUE (kind, ref)
);

CREATE TABLE search_state (
key    TEXT NOT NULL PRIMARY KEY,
    value  TEXT NOT NULL
);

CREATE TABLE searches (
id          INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id  BLOB CHECK (session_id IS NULL OR length(session_id)=16) REFERENCES sessions(id),
    query       TEXT NOT NULL,
    scope       TEXT NOT NULL,
    results     TEXT NOT NULL,
    reranked    INTEGER NOT NULL DEFAULT false,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
);

CREATE TABLE made_tools (
name         TEXT NOT NULL PRIMARY KEY,
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
    approved_at  TEXT,
    approved_sha TEXT,
    rejected_at  TEXT
);

CREATE TABLE jobs (
id            INTEGER PRIMARY KEY AUTOINCREMENT,
    name          TEXT NOT NULL,
    kind          TEXT NOT NULL CHECK (kind IN ('agent', 'system', 'script')),
    action        TEXT,
    prompt        TEXT,
    context       TEXT NOT NULL DEFAULT '[]',
    skills        TEXT NOT NULL DEFAULT '[]',
    schedule      TEXT NOT NULL,
    tz            TEXT NOT NULL,
    model         TEXT,
    workspace     TEXT,
    enabled       INTEGER NOT NULL DEFAULT true,
    paused_reason TEXT,
    created_by    TEXT NOT NULL,
    created_in    BLOB CHECK (created_in IS NULL OR length(created_in)=16),
    approval      TEXT,
    next_run_at   TEXT,
    last_run_at   TEXT,
    last_status   TEXT,
    last_error    TEXT,
    failures      INTEGER NOT NULL DEFAULT 0,
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
    updated_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
    removed_at    TEXT,
report_file TEXT,
post_to BLOB CHECK (post_to IS NULL OR length(post_to)=16),
owner_approved INTEGER NOT NULL DEFAULT false,
command text
);

CREATE TABLE job_runs (
id          INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id      INTEGER NOT NULL REFERENCES jobs (id),
    trigger     TEXT NOT NULL,
    status      TEXT NOT NULL,
    session_id  BLOB CHECK (session_id IS NULL OR length(session_id)=16),
    started_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
    ended_at    TEXT,
    output      TEXT,
    error       TEXT,
tainted INTEGER NOT NULL DEFAULT false,
posted_to BLOB CHECK (posted_to IS NULL OR length(posted_to)=16),
pending_to BLOB CHECK (pending_to IS NULL OR length(pending_to)=16),
risk INTEGER NOT NULL DEFAULT 4 CHECK (risk BETWEEN 0 AND 5),
rating_reason text NOT NULL DEFAULT 'unknown',
rating_writer_risk INTEGER,
command text,
exit_code integer
);

CREATE TABLE prompt_suggestions (
id             INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id     BLOB CHECK (session_id IS NULL OR length(session_id)=16) NOT NULL REFERENCES sessions(id),
    after_seq      INTEGER NOT NULL,
    suggested      TEXT NOT NULL,
    final          TEXT,
    outcome        TEXT CHECK (outcome IN ('accepted', 'edited', 'declined', 'unseen')),
    model          TEXT NOT NULL,
    prompt_version TEXT NOT NULL,
    latency_ms     INTEGER,
    cost_usd       REAL,
    created_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
    decided_at     TEXT
);

CREATE TABLE search_queue (
id    INTEGER PRIMARY KEY AUTOINCREMENT,
    kind  TEXT NOT NULL,
    ref   INTEGER NOT NULL
);

CREATE TABLE draft_taint (
skill       TEXT NOT NULL PRIMARY KEY,
    session_id  BLOB CHECK (session_id IS NULL OR length(session_id)=16),
    at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
);

CREATE TABLE action_gates (
id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id BLOB CHECK (session_id IS NULL OR length(session_id)=16) NOT NULL REFERENCES sessions(id),
    action TEXT NOT NULL,
    reason TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
    allowed_at TEXT,
    used_at TEXT,
decision text CHECK (decision IN ('approved','rejected','denied')),
resolved_at TEXT,
s1_decision INTEGER
);

CREATE TABLE sources (
source text NOT NULL PRIMARY KEY,
    risk INTEGER NOT NULL CHECK (risk IN (2,3)),
    note text NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
);

CREATE TABLE source_audit (
id INTEGER PRIMARY KEY AUTOINCREMENT,
    source text NOT NULL,
    action text NOT NULL,
    risk INTEGER,
    note text NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
);

CREATE TABLE source_vouches (
session_id BLOB CHECK (session_id IS NULL OR length(session_id)=16) NOT NULL REFERENCES sessions(id),
    owner_seq integer NOT NULL,
    url text NOT NULL,
    base INTEGER NOT NULL,
    risk INTEGER NOT NULL CHECK (risk BETWEEN 2 AND 4),
    choice text NOT NULL,
    PRIMARY KEY (session_id, owner_seq, url, base)
);

CREATE TABLE item_ratings (
id INTEGER PRIMARY KEY AUTOINCREMENT,
    path text NOT NULL,
    sha256 text NOT NULL,
    session_id BLOB CHECK (session_id IS NULL OR length(session_id)=16) REFERENCES sessions(id),
    risk INTEGER NOT NULL CHECK (risk BETWEEN 0 AND 5),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
    UNIQUE(path,sha256,session_id)
);

CREATE TABLE contamination (
cause BLOB CHECK (cause IS NULL OR length(cause)=16) NOT NULL REFERENCES sessions(id),
    affected BLOB CHECK (affected IS NULL OR length(affected)=16) NOT NULL REFERENCES sessions(id),
    at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
    cleared_at TEXT,
    PRIMARY KEY(cause,affected)
);

CREATE TABLE session_inputs (
reader BLOB CHECK (reader IS NULL OR length(reader)=16) NOT NULL REFERENCES sessions(id),
    writer BLOB CHECK (writer IS NULL OR length(writer)=16) NOT NULL REFERENCES sessions(id),
    PRIMARY KEY(reader,writer),
    CHECK(reader != writer)
);

CREATE TABLE source_ratings (
scope text NOT NULL PRIMARY KEY,
 risk INTEGER NOT NULL CHECK (risk BETWEEN 2 AND 4),
 answers TEXT NOT NULL,
 model text NOT NULL,
 rated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
 proposal text CHECK (proposal IN ('pending','approved','rejected')),
 proposed_at TEXT,
 snapshot BLOB CHECK (snapshot IS NULL OR length(snapshot)=16) NOT NULL
);

CREATE TABLE source_proposals (
id INTEGER PRIMARY KEY AUTOINCREMENT,
 scope text NOT NULL,
 risk INTEGER NOT NULL CHECK (risk=2),
 snapshot BLOB CHECK (snapshot IS NULL OR length(snapshot)=16) NOT NULL UNIQUE,
 created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
 decision text CHECK (decision IN ('approved','rejected')),
 decided_at TEXT
);

CREATE TABLE source_rerates (
version integer PRIMARY KEY,
 completed_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
 note text NOT NULL
);

CREATE TABLE s1_states (
decision_id INTEGER PRIMARY KEY REFERENCES decisions(id) ON DELETE CASCADE,
    state TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
);

CREATE TABLE s1_outcomes (
id INTEGER PRIMARY KEY AUTOINCREMENT,
    decision_id INTEGER NOT NULL REFERENCES decisions(id) ON DELETE CASCADE,
    correct INTEGER NOT NULL,
    truth text,
    source text NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
    UNIQUE (decision_id, source)
);

CREATE TABLE s1_calibration (
point text NOT NULL,
    model text NOT NULL,
    question text NOT NULL,
    curve TEXT NOT NULL,
    labelled integer NOT NULL,
    fitted_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
    PRIMARY KEY (point, model, question)
);

CREATE TABLE s1_proposals (
id INTEGER PRIMARY KEY AUTOINCREMENT,
    point text NOT NULL,
    kind text NOT NULL,
    summary text NOT NULL,
    change TEXT NOT NULL,
    status text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','approved','rejected','stale')),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
    decided_at TEXT
);

CREATE TABLE egress (
session_id BLOB CHECK (session_id IS NULL OR length(session_id)=16) NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    host text NOT NULL,
    rating INTEGER NOT NULL,
    outcome text NOT NULL,
    count integer NOT NULL DEFAULT 1,
    first_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
    last_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
    PRIMARY KEY (session_id, host)
);

CREATE TABLE pending_prompts (
id INTEGER PRIMARY KEY AUTOINCREMENT,
 session_id BLOB CHECK (session_id IS NULL OR length(session_id)=16) NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
 text text NOT NULL,
 steer INTEGER NOT NULL DEFAULT false,
 created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
 error text
);

CREATE TABLE tape_exports (
session_id BLOB CHECK (session_id IS NULL OR length(session_id)=16) NOT NULL PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
 version INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE brain_memory_inputs (
memory_id INTEGER NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
 writer BLOB CHECK (writer IS NULL OR length(writer)=16) NOT NULL REFERENCES sessions(id),
 PRIMARY KEY(memory_id,writer)
);

CREATE TABLE brain_records (
id text NOT NULL PRIMARY KEY, store text NOT NULL, kind text NOT NULL, title text NOT NULL,
 path text NOT NULL UNIQUE, digest text NOT NULL, modified TEXT NOT NULL,
 metadata TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE brain_aliases (
alias text NOT NULL, record_id text NOT NULL, PRIMARY KEY(alias,record_id)
);

CREATE TABLE brain_links (
source text NOT NULL, relation text NOT NULL, target text NOT NULL, PRIMARY KEY(source,relation,target)
);

CREATE TABLE brain_skill_uses (
id INTEGER PRIMARY KEY AUTOINCREMENT, session_id BLOB CHECK (session_id IS NULL OR length(session_id)=16) NOT NULL REFERENCES sessions(id),
 name text NOT NULL, digest text NOT NULL, outcome text, at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
);

CREATE TABLE mcp_interactions (
id BLOB CHECK (id IS NULL OR length(id)=16) NOT NULL PRIMARY KEY,
 session_id BLOB CHECK (session_id IS NULL OR length(session_id)=16) NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
 server text NOT NULL,
 request_id TEXT NOT NULL,
 message text NOT NULL,
 schema TEXT NOT NULL,
 status text NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','accept','decline','cancel')),
 content TEXT,
 created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
 expires_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now','+5 minutes'))
);

CREATE INDEX tape_events_session ON tape_events(session_id, id);

CREATE INDEX turns_session ON turns(session_id, started_at);

CREATE INDEX model_calls_turn ON model_calls(turn_id);

CREATE INDEX tool_calls_turn ON tool_calls(turn_id);

CREATE INDEX session_decisions_session ON session_decisions(session_id, created_at);

CREATE INDEX session_scores_session ON session_scores(session_id, created_at);

CREATE UNIQUE INDEX tape_events_seq ON tape_events(session_id, seq);

CREATE INDEX compactions_session ON compactions(session_id, created_at);

CREATE INDEX decisions_session ON decisions(session_id, created_at);

CREATE INDEX IF NOT EXISTS model_calls_session ON model_calls(session_id);

CREATE INDEX IF NOT EXISTS tool_calls_session ON tool_calls(session_id);

CREATE INDEX IF NOT EXISTS tape_events_kind ON tape_events(session_id, kind, seq);

CREATE INDEX IF NOT EXISTS session_scores_turn ON session_scores(turn_id, trigger);

CREATE INDEX IF NOT EXISTS model_calls_session_untraced ON model_calls(session_id) WHERE turn_id IS NULL;

CREATE INDEX memories_tier ON memories(tier, updated_at);

CREATE INDEX search_docs_unembedded ON search_docs (id) WHERE embedding IS NULL;

CREATE UNIQUE INDEX jobs_name ON jobs (name) WHERE removed_at IS NULL;

CREATE INDEX jobs_due ON jobs (next_run_at) WHERE enabled AND removed_at IS NULL;

CREATE UNIQUE INDEX job_runs_one_running ON job_runs (job_id) WHERE status = 'running';

CREATE INDEX job_runs_by_job ON job_runs (job_id, started_at DESC);

CREATE INDEX prompt_suggestions_session ON prompt_suggestions (session_id, id);

CREATE INDEX search_docs_to_embed ON search_docs (id) WHERE embedding IS NULL AND embed_attempts < 3;

CREATE INDEX tool_calls_name ON tool_calls (name);

CREATE INDEX action_gates_session ON action_gates (session_id, id DESC);

CREATE INDEX item_ratings_path ON item_ratings(path,sha256);

CREATE INDEX contamination_affected ON contamination(affected) WHERE cleared_at IS NULL;

CREATE INDEX session_inputs_writer ON session_inputs(writer);

CREATE INDEX item_ratings_digest ON item_ratings(sha256);

CREATE INDEX action_gates_pending ON action_gates(created_at DESC) WHERE decision IS NULL AND allowed_at IS NULL;

CREATE INDEX decisions_point ON decisions(point, created_at);

CREATE INDEX s1_states_created ON s1_states(created_at);

CREATE UNIQUE INDEX s1_proposals_open ON s1_proposals(point, kind) WHERE status = 'pending';

CREATE INDEX pending_prompts_session ON pending_prompts(session_id,id) WHERE error IS NULL;

CREATE INDEX brain_aliases_record ON brain_aliases(record_id);

CREATE INDEX brain_links_target ON brain_links(target);

CREATE INDEX mcp_interactions_pending ON mcp_interactions(session_id) WHERE status='pending';

-- Start at live causes and follow readers, rather than materializing every
-- session/ancestor pair. UNION terminates dependency cycles and deduplicates readers.
CREATE VIEW zen_contaminated_sessions AS
WITH RECURSIVE edges(reader,writer) AS (
 SELECT id,parent FROM sessions WHERE parent IS NOT NULL
 UNION SELECT reader,writer FROM session_inputs
), bad(id) AS (
 SELECT affected FROM contamination WHERE cleared_at IS NULL
 UNION SELECT e.reader FROM edges e JOIN bad b ON e.writer=b.id
)
SELECT id FROM bad;
CREATE VIEW zen_contaminated_memories AS
SELECT DISTINCT b.memory_id AS id FROM brain_memory_inputs b
JOIN zen_contaminated_sessions s ON s.id=b.writer;

CREATE TRIGGER memories_search_insert AFTER INSERT ON memories BEGIN
 INSERT INTO search_queue(kind,ref) VALUES('memory',NEW.id);
END;
CREATE TRIGGER memories_search_delete AFTER DELETE ON memories BEGIN
 INSERT INTO search_queue(kind,ref) VALUES('memory',OLD.id);
END;
CREATE TRIGGER memories_search_update AFTER UPDATE OF text,tier,source ON memories
WHEN OLD.text IS NOT NEW.text OR OLD.tier IS NOT NEW.tier OR OLD.source IS NOT NEW.source BEGIN
 INSERT INTO search_queue(kind,ref) VALUES('memory',NEW.id);
END;

-- SQLite cannot assign NEW columns in BEFORE triggers. A single AFTER trigger
-- authenticates both risk and evidence atomically; RETURNING callers must reread.
CREATE TRIGGER memories_origin_insert AFTER INSERT ON memories BEGIN
 UPDATE memories SET
 risk=CASE WHEN EXISTS(SELECT 1 FROM tape_events WHERE session_id=NEW.session_id
  AND origin='owner' AND risk=0 AND kind='message'
  AND json_extract(payload,'$.role')='user' AND json_extract(payload,'$.content')=NEW.text)
  THEN 0 ELSE coalesce((SELECT risk FROM sessions WHERE id=NEW.session_id),4) END,
 rating_writer_risk=(SELECT risk FROM sessions WHERE id=NEW.session_id),
 rating_reason=CASE WHEN EXISTS(SELECT 1 FROM tape_events WHERE session_id=NEW.session_id
  AND origin='owner' AND risk=0 AND kind='message'
  AND json_extract(payload,'$.role')='user' AND json_extract(payload,'$.content')=NEW.text)
  THEN 'owner' WHEN EXISTS(SELECT 1 FROM sessions WHERE id=NEW.session_id) THEN 'writer_session' ELSE 'unknown' END
 WHERE id=NEW.id;
 INSERT INTO brain_memory_inputs(memory_id,writer) SELECT NEW.id,NEW.session_id WHERE NEW.session_id IS NOT NULL ON CONFLICT DO NOTHING;
END;
CREATE TRIGGER memories_origin_update AFTER UPDATE OF text,session_id ON memories BEGIN
 UPDATE memories SET
 risk=CASE WHEN EXISTS(SELECT 1 FROM tape_events WHERE session_id=NEW.session_id
  AND origin='owner' AND risk=0 AND kind='message'
  AND json_extract(payload,'$.role')='user' AND json_extract(payload,'$.content')=NEW.text)
  THEN 0 ELSE coalesce((SELECT risk FROM sessions WHERE id=NEW.session_id),4) END,
 rating_writer_risk=(SELECT risk FROM sessions WHERE id=NEW.session_id),
 rating_reason=CASE WHEN EXISTS(SELECT 1 FROM tape_events WHERE session_id=NEW.session_id
  AND origin='owner' AND risk=0 AND kind='message'
  AND json_extract(payload,'$.role')='user' AND json_extract(payload,'$.content')=NEW.text)
  THEN 'owner' WHEN EXISTS(SELECT 1 FROM sessions WHERE id=NEW.session_id) THEN 'writer_session' ELSE 'unknown' END
 WHERE id=NEW.id;
 INSERT INTO brain_memory_inputs(memory_id,writer) SELECT NEW.id,NEW.session_id WHERE NEW.session_id IS NOT NULL ON CONFLICT DO NOTHING;
END;
CREATE TRIGGER job_runs_origin_insert AFTER INSERT ON job_runs BEGIN
 UPDATE job_runs SET risk=coalesce((SELECT risk FROM sessions WHERE id=NEW.session_id),4),
 rating_writer_risk=(SELECT risk FROM sessions WHERE id=NEW.session_id),
 rating_reason=CASE WHEN EXISTS(SELECT 1 FROM sessions WHERE id=NEW.session_id) THEN 'writer_session' ELSE 'unknown' END WHERE id=NEW.id;
END;
CREATE TRIGGER job_runs_origin_update AFTER UPDATE OF output,session_id ON job_runs BEGIN
 UPDATE job_runs SET risk=coalesce((SELECT risk FROM sessions WHERE id=NEW.session_id),4),
 rating_writer_risk=(SELECT risk FROM sessions WHERE id=NEW.session_id),
 rating_reason=CASE WHEN EXISTS(SELECT 1 FROM sessions WHERE id=NEW.session_id) THEN 'writer_session' ELSE 'unknown' END WHERE id=NEW.id;
END;
CREATE TRIGGER tape_export_insert AFTER INSERT ON tape_events BEGIN
 INSERT INTO tape_exports(session_id) VALUES(NEW.session_id)
 ON CONFLICT(session_id) DO UPDATE SET version=version+1;
END;
CREATE TRIGGER tape_export_update AFTER UPDATE ON tape_events BEGIN
 INSERT INTO tape_exports(session_id) VALUES(NEW.session_id)
 ON CONFLICT(session_id) DO UPDATE SET version=version+1;
END;

-- Identifier preserving Unicode tokenizer; semantic distance uses bundled sqlite-vec.
CREATE VIRTUAL TABLE search_docs_fts USING fts5(ident,title,body,
 content='search_docs',content_rowid='id',tokenize='unicode61 remove_diacritics 2');
CREATE TRIGGER search_docs_fts_insert AFTER INSERT ON search_docs BEGIN
 INSERT INTO search_docs_fts(rowid,ident,title,body) VALUES(NEW.id,NEW.ident,NEW.title,NEW.body);
END;
CREATE TRIGGER search_docs_fts_delete AFTER DELETE ON search_docs BEGIN
 INSERT INTO search_docs_fts(search_docs_fts,rowid,ident,title,body) VALUES('delete',OLD.id,OLD.ident,OLD.title,OLD.body);
END;
CREATE TRIGGER search_docs_fts_update AFTER UPDATE OF ident,title,body ON search_docs BEGIN
 INSERT INTO search_docs_fts(search_docs_fts,rowid,ident,title,body) VALUES('delete',OLD.id,OLD.ident,OLD.title,OLD.body);
 INSERT INTO search_docs_fts(rowid,ident,title,body) VALUES(NEW.id,NEW.ident,NEW.title,NEW.body);
END;
