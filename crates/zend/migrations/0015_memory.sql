-- Memory (DESIGN.md, "Memory and knowledge"). Short-term entries are rendered into MEMORY.md at
-- session start; a nightly sleep keeps, drops (archives) or promotes them. Nothing is deleted.
CREATE TABLE memories (
    id          BIGSERIAL PRIMARY KEY,
    text        TEXT NOT NULL,
    source      TEXT NOT NULL DEFAULT 'inferred',  -- owner (their words) | verified (a checked result) | inferred
    tier        TEXT NOT NULL DEFAULT 'short',     -- short | long | archived
    session_id  UUID REFERENCES sessions(id),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    used_at     TIMESTAMPTZ,                       -- last time a search returned it or the model edited it
    uses        INT NOT NULL DEFAULT 0,
    scores      JSONB,                             -- the latest sleep's System One answers
    proposed    TEXT,                              -- what a sleep proposed but didn't do: promote | user
    reason      TEXT,                              -- why it last changed tier
    supersedes  BIGINT REFERENCES memories(id)
);
CREATE INDEX memories_tier ON memories(tier, updated_at);

-- One row per sleep: what it did, so the owner can see it the next morning.
CREATE TABLE sleep_runs (
    id          BIGSERIAL PRIMARY KEY,
    started_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    ended_at    TIMESTAMPTZ,
    trigger     TEXT NOT NULL,                     -- nightly | ceiling | owner
    scorer      TEXT,                              -- the System One model, or null (recency only)
    entries     INT,
    kept        INT,
    dropped     INT,
    promoted    INT,
    proposed    INT,
    note        TEXT,
    error       TEXT
);
