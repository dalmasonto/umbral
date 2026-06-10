# Missing features

Django-parity gaps grounded in an actual in-tree need (not speculation). Broad, already-tracked gaps live in `bugs/features.md`, `gaps.md`, `gaps2.md`, `REAL-GAPS.md` — see the "already tracked" list in [`broken-features.md`](broken-features.md). Only the evidence-grounded new gap is below.

---

## MISS-1 — No `select_for_update()` / `skip_locked()` anywhere in the ORM
**Severity: medium** · **Verified** (zero hits for `FOR UPDATE` / `for_update` / `skip_locked` in `crates/umbra-core/src`)

- **Evidence:** The ORM has no row-locking primitive. This isn't a theoretical gap — it's the exact operation `umbra-tasks` needed and worked around incorrectly (see [BROKEN-1](broken-features.md)). The tasks `claim_one` wraps a read-then-update in a transaction and *claims in a comment* that this prevents Postgres double-claims, but without `FOR UPDATE SKIP LOCKED` it doesn't.
- **Why it matters:** Any worker-queue, counter, or "claim a row" pattern on Postgres needs `SELECT … FOR UPDATE` (optionally `SKIP LOCKED`) to be correct under concurrency. It's also absent from the features.md ORM list (#13-38) and the gaps files. Per CLAUDE.md's own rule — *"If the ORM can't express a row-level operation you need, the right fix is to add the operation to the ORM"* — this is the gap entry the tasks plugin should have filed instead of the raw workaround.
- **Fix:** Add `QuerySet::select_for_update()` and `.skip_locked()`:
  - Postgres: append `FOR UPDATE` / `FOR UPDATE SKIP LOCKED` to the SELECT.
  - SQLite: documented no-op (single-writer model makes it harmless — the same dispatch shape as other backend-specific ORM features).
  - Then rewrite `umbra-tasks::claim_one` to use it and delete the incorrect comment.

---

## Note on scope

The broken-features audit deliberately did **not** generate a long Django-parity wishlist — the repo already tracks those in `features.md`/`gaps.md` and the project's build order (`arch.md §8`) sequences them intentionally. The only new "missing" item reported is the one with a concrete in-tree consumer (tasks) and a CLAUDE.md rule pointing straight at it. Everything else a Django dev might miss (multipart uploads, periodic task scheduling, test factories, `collect_static`, etc.) is already logged — cross-referenced in the "already tracked" table.
