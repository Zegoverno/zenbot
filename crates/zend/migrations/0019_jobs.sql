-- Scheduled jobs (jobs.rs, D-046): the kernel's own (`system`: the memory sleep, engine updates) and
-- agent jobs (a prompt run in a fresh session on a schedule). Definition and scheduler state live in
-- one row; only the scheduler writes the state columns.
CREATE TABLE jobs (
    id            BIGSERIAL PRIMARY KEY,
    name          TEXT NOT NULL,
    kind          TEXT NOT NULL CHECK (kind IN ('agent', 'system')),
    action        TEXT,                              -- system jobs: what the kernel runs (`sleep`, `engines`)
    prompt        TEXT,                              -- agent jobs: the task
    context       TEXT[] NOT NULL DEFAULT '{}',      -- prompt files in its instructions (soul, identity, agents, user, memory)
    skills        TEXT[] NOT NULL DEFAULT '{}',      -- skills loaded into its instructions
    schedule      TEXT NOT NULL,                     -- cron expression, `every 2h`, or `at <time>`
    tz            TEXT NOT NULL,                     -- IANA zone the schedule is read in
    model         TEXT,                              -- NULL: the default model when it runs
    workspace     TEXT,
    enabled       BOOLEAN NOT NULL DEFAULT true,
    paused_reason TEXT,                              -- why it isn't enabled (owner, approval pending, failures)
    created_by    TEXT NOT NULL,                     -- owner | agent | kernel
    created_in    UUID,                              -- the session that created it
    approval      JSONB,                             -- System One's judgment of an agent-created job
    next_run_at   TIMESTAMPTZ,                       -- NULL: nothing more to run (a one-shot that ran)
    last_run_at   TIMESTAMPTZ,
    last_status   TEXT,
    last_error    TEXT,
    failures      INT NOT NULL DEFAULT 0,            -- consecutive failed runs
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    removed_at    TIMESTAMPTZ                        -- removed jobs stay for their runs' history
);
CREATE UNIQUE INDEX jobs_name ON jobs (name) WHERE removed_at IS NULL;
CREATE INDEX jobs_due ON jobs (next_run_at) WHERE enabled AND removed_at IS NULL;

CREATE TABLE job_runs (
    id          BIGSERIAL PRIMARY KEY,
    job_id      BIGINT NOT NULL REFERENCES jobs (id),
    trigger     TEXT NOT NULL,                       -- schedule | catchup | owner | agent
    status      TEXT NOT NULL,                       -- running | ok | silent | error | interrupted
    session_id  UUID,                                -- agent jobs: the session it ran in
    started_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    ended_at    TIMESTAMPTZ,
    output      TEXT,                                -- the report (agent) or the result line (system)
    error       TEXT
);
-- At most one run of a job at a time, enforced by the database.
CREATE UNIQUE INDEX job_runs_one_running ON job_runs (job_id) WHERE status = 'running';
CREATE INDEX job_runs_by_job ON job_runs (job_id, started_at DESC);
