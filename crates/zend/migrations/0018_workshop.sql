-- Tools the agent made (workshop.rs): the owner's approval lives here, not in the tool's folder the
-- agent can write, so only the kernel can let a tool reach the network.
CREATE TABLE made_tools (
    name         TEXT PRIMARY KEY,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    approved_at  TIMESTAMPTZ,
    approved_sha TEXT,                         -- the tool's content (manifest and files) when approved
    rejected_at  TIMESTAMPTZ
);
