## D-028 — Memory: fixed-size short-term, nightly sleep, a very high bar for long-term

> Long-term promotion superseded by D-045 (2026-10-08): the agent writes traits and guidance into
> `IDENTITY.md` or `USER.md` itself, and the sleep keeps, archives or promotes into them and the
> wiki. Paths moved by D-040.

**Decision:** Anything can be saved to short-term memory (`MEMORY.md`, fixed size, rendered at session
start and frozen for the session). A nightly sleep job asks System One about every entry (needed
soon? durable? impactful across jobs? already covered?) and keeps, drops (archived, never deleted) or
promotes. Entries compete for the fixed space. Only really impactful memories reach long-term: very
high probability (e.g. ≥ 0.95) on the lower end of the confidence interval, from the owner's words or
a verified result, not already covered. Facts about the owner become proposed `USER.md` edits.
Promotion runs in shadow mode until long-term memory has a reader (search, Phase 3). Knowledge goes to
a wiki (`capture`, System One routes and de-duplicates); search is full-text then semantic over all of
it; the web through `web_search` (swappable providers) and `web_fetch`.

**Why:** The owner's model: short-term holds what matters now; long-term is earned. A fixed size
forces prioritization. Freezing per session keeps the prompt cache.

**Date:** 2026-10-06

---
