-- Session names and next-prompt suggestions (assist.rs, D-047).
-- Where a session's title came from: `auto` (the first prompt, cut), `model` (named by the assist
-- model after a turn) or `owner` (set by hand). The assist model renames only `auto` and `model`
-- titles, so a name the owner set is never overwritten.
ALTER TABLE sessions ADD COLUMN title_source TEXT NOT NULL DEFAULT 'auto';

-- One row per suggested next prompt, with what the owner did with it: `accepted` (sent as is),
-- `edited` (taken, changed, then sent: `final` holds what was sent), `declined` (the owner wrote
-- their own: `final`), `unseen` (the next prompt came from a client that doesn't show suggestions).
-- NULL outcome: no next prompt yet.
CREATE TABLE prompt_suggestions (
    id             BIGSERIAL PRIMARY KEY,
    session_id     UUID NOT NULL REFERENCES sessions(id),
    after_seq      INT NOT NULL,                -- the last message block the suggestion follows
    suggested      TEXT NOT NULL,
    final          TEXT,
    outcome        TEXT CHECK (outcome IN ('accepted', 'edited', 'declined', 'unseen')),
    model          TEXT NOT NULL,
    prompt_version TEXT NOT NULL,               -- the assist prompt that wrote it, to compare versions
    latency_ms     INT,
    cost_usd       DOUBLE PRECISION,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    decided_at     TIMESTAMPTZ
);
CREATE INDEX prompt_suggestions_session ON prompt_suggestions (session_id, id);
