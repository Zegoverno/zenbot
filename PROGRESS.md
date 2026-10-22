# zenbot — Progress Log

> Append-only. What shipped, what we learned, what changed and why. Newest entry at the top.
> Add an entry with every pull request that ships something; keep entries lean. Decisions go to
> DECISIONS.md, plans to ROADMAP.md.

---

## 2026-10-22 — Phase 10: docs rewrite

The reference docs are rewritten against the code at about a quarter of their size: protocols keep
every message and field, ROADMAP keeps only open work and debt that is still true.

## 2026-10-22 — Phases 9 and 10: visible text, self-upgrading Matrix, aarch64 (D-077)

The terminal and Matrix drop invisible characters that could hide or reorder text. `upgrade.sh`
rebuilds and restarts the Matrix bridge when its code changed, with rollback. CI publishes aarch64
builds too, and the installer picks the right Node, so zenbot installs on a 64-bit Raspberry Pi.

## 2026-10-22 — Complete PostgreSQL upgrade preparation

After a copied PostgreSQL snapshot passes the smoke turn, upgrade preparation reports the pending
backend cutover instead of asking the retained PostgreSQL source for SQLite migration versions.
The detached installer still takes the final quiescent archive before changing the backend.

## 2026-10-22 — Preserve all-word search and exercise SQLite fixtures

Full-text search requires every literal query word, preserving PostgreSQL’s ordinary multiword
behavior so typed-link expansion still brings in related records. A partial-word regression guards
this behavior. End-to-end fixtures use SQLite UUID blobs, booleans and failure-injection triggers.

## 2026-10-22 — Account for the bundled SQLite binary

The kernel release size budget includes SQLite, FTS5 and sqlite-vec: measured at 22,974 KiB
locally and 22,965 KiB in CI, with about 15% headroom. Client and worker budgets stay fixed.

## 2026-10-22 — Retain the PostgreSQL project during staged cutover

Staged upgrades select the original Compose project when exporting PostgreSQL, so moving the
Compose file beside tested binaries still reaches the retained source. Cutover regression coverage
checks the exact exporter command; storage and CI instructions match the SQLite implementation.

## 2026-10-22 — Phase 8: SQLite state and portable archives (D-076)

The kernel uses a private SQLite database with bundled sqlite-vec and FTS5, without a Docker
runtime requirement. Existing PostgreSQL state migrates through a versioned JSONL snapshot:
IDs, vectors, provenance and historical tape hashes survive; validation precedes publication.
Consistent SQLite backups include WAL. Search can use an external SearXNG or Brave/Tavily.
