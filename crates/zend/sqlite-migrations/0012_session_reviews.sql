-- The learning loop's reviews (review.rs): after the owner archives a session with an outcome, a
-- `review` session judges how the work went and records it here with `submit_review`.
CREATE TABLE session_reviews (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id        BLOB NOT NULL CHECK (length(session_id) = 16) REFERENCES sessions(id),
    review_session_id BLOB CHECK (review_session_id IS NULL OR length(review_session_id) = 16) REFERENCES sessions(id),
    agent             TEXT NOT NULL,
    outcome           TEXT NOT NULL CHECK (outcome IN ('done', 'partial', 'not-done')),
    kind_of_work      TEXT NOT NULL,
    intervention      TEXT NOT NULL CHECK (intervention IN ('none', 'low', 'medium', 'high')),
    model_verdict     TEXT NOT NULL CHECK (model_verdict IN ('right', 'too-weak', 'more-than-needed')),
    summary           TEXT NOT NULL,
    evidence          TEXT NOT NULL DEFAULT '',
    changes           TEXT NOT NULL DEFAULT '[]',
    suggestions       TEXT NOT NULL DEFAULT '[]',
    created_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
);
CREATE INDEX session_reviews_agent ON session_reviews (agent, id);
