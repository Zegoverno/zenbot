# zenbot — Progress Log

> Append-only. What shipped, what we learned, what changed and why. Newest entry at the top.
> Add an entry with every pull request that ships something; keep entries lean. Decisions go to
> DECISIONS.md, plans to ROADMAP.md.

---

## 2026-10-22 — Phases 9 and 10: visible text, self-upgrading Matrix, aarch64 (D-077)

The terminal and Matrix drop invisible characters that could hide or reorder text. `upgrade.sh`
rebuilds and restarts the Matrix bridge when its code changed, with rollback. CI publishes aarch64
builds too, and the installer picks the right Node, so zenbot installs on a 64-bit Raspberry Pi.
