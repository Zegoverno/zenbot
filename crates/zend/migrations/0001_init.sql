CREATE TABLE sessions (
    id          UUID PRIMARY KEY,
    title       TEXT NOT NULL DEFAULT '',
    model       TEXT NOT NULL,
    archived    BOOLEAN NOT NULL DEFAULT FALSE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Append-only history. The model reads only what the tape contains.
CREATE TABLE tape_events (
    id          BIGSERIAL PRIMARY KEY,
    session_id  UUID NOT NULL REFERENCES sessions(id),
    kind        TEXT NOT NULL,
    payload     JSONB NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX tape_events_session ON tape_events(session_id, id);

CREATE TABLE model_calls (
    id            BIGSERIAL PRIMARY KEY,
    session_id    UUID NOT NULL REFERENCES sessions(id),
    provider      TEXT NOT NULL,
    model         TEXT NOT NULL,
    input_tokens  BIGINT NOT NULL,
    output_tokens BIGINT NOT NULL,
    cache_read    BIGINT NOT NULL,
    cache_write   BIGINT NOT NULL,
    cost_usd      DOUBLE PRECISION NOT NULL,
    stop_reason   TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE tool_calls (
    id           BIGSERIAL PRIMARY KEY,
    session_id   UUID NOT NULL REFERENCES sessions(id),
    call_id      TEXT NOT NULL,
    name         TEXT NOT NULL,
    args         JSONB NOT NULL,
    is_error     BOOLEAN NOT NULL,
    duration_ms  BIGINT NOT NULL,
    output_bytes BIGINT NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
