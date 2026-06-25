done hardening

# Race-condition & concurrent-write audit

Scope: can two concurrent requests produce diverged or corrupted data? Read against the real code; `file:line` cited. READ-ONLY review — nothing fixed.

Backstop landscape (matters for every finding below):
- SQLite pools are opened WAL + `busy_timeout=5s` + `foreign_keys=ON` (`crates/umbral-core/src/db.rs:326-357`). WAL gives one writer at a time; the busy-timeout serialises contended writers rather than erroring instantly. Postgres pools get a bounded acquire timeout but default READ COMMITTED isolation.
- The typed `create()` and dynamic `insert_json()` paths both run the DB statement and then `classify_sql_error` (`crates/umbral-core/src/orm/validation.rs:167`) to turn a UNIQUE/FK/NOT-NULL/CHECK violation into a structured `WriteError`. So a SELECT-precheck that loses a race still hits the DB constraint as a backstop — *as long as the constraint exists*.

---

## Sessions

**Critical:** `set_data` read-modify-writes the whole `data` JSON blob — concurrent writes lose keys (`plugins/umbral-sessions/src/lib.rs:400-456`).
- Interleaving: A `SELECT data` → `{}`. B `SELECT data` → `{}`. A sets `cart=…`, writes `{"cart":…}`. B sets `flash=…`, writes `{"flash":…}` (B never saw A's cart). Last writer wins the whole column.
- Consequence: lost update. Two near-simultaneous requests on the same cookie (e.g. an XHR + a form post, or a double-click) silently drop one's session mutation. Flash messages vanish, cart items disappear, CSRF-rotation state can be clobbered.
- Why the existing guard doesn't help: the lazy-create branch (`:426-444`) correctly tolerates the *row-creation* race via `UniqueViolation` re-read, but the actual `data` merge is still a full-blob overwrite with no `SELECT … FOR UPDATE`, no optimistic version column, and no JSON-patch UPDATE.
- Fix: either (a) wrap read+merge+write in a transaction with `SELECT … FOR UPDATE` on the row (Postgres) / rely on WAL single-writer + immediate-tx (SQLite), or (b) move to a server-side JSON merge (`json_set`/`jsonb_set`) via a new ORM `update_json_merge` op so the merge happens atomically in one UPDATE. gaps ref: NEW.

**Important:** `read_session` lazy-expiry delete races a concurrent refresh, but is benign (`plugins/umbral-sessions/src/lib.rs:243-256`). A expires-and-deletes while B reads the same row; B may still return a row A just deleted, or both delete. Worst case is one extra anonymous round; no corruption. Noted so it isn't re-audited. gaps ref: none.

**Safe:** session-fixation rotation in `login_user_id` (`:485-528`) destroys the old token and mints a fresh UUID-v4 token, so there's no shared key to race on. The carry-over `update_values` targets the brand-new row only.

---

## Membership / junction idempotency (umbral-permissions)

**Important (guarded):** `add_user_to_group` / `grant_user_permission` do `if exists() { return } … create()` (`plugins/umbral-permissions/src/membership.rs:40-52, 130-142`). The SELECT→INSERT gap is real, but `UserGroup`/`UserPermission` carry `unique_together = [["user_id","group_id"]]` / `[["user_id","permission_id"]]` (`plugins/umbral-permissions/src/models.rs:218,248`), so a duplicate INSERT that wins the race hits the DB UNIQUE constraint. The problem: `create()` surfaces that as `WriteError::UniqueViolation` and `add_user_to_group` does **not** catch it — the second concurrent caller gets an error return instead of the documented idempotent `Ok(())`. Consequence: no duplicate row (data stays correct), but a spurious 500/error on a legitimately-idempotent op under concurrency. Fix: match `Err(UniqueViolation)` → `Ok(())` like `set_data` does for its create. gaps ref: NEW.

**Critical:** `set_user_groups` is `delete()` then `bulk_create()` as two separate auto-commit statements — no transaction (`plugins/umbral-permissions/src/membership.rs:77-97`).
- Interleaving: A DELETEs user's rows (commits). A's `bulk_create` is in flight. B's `groups_for_user` / permission check runs now → sees **zero** groups → denies access / renders empty. Then A's insert commits. Worse: A `delete`+`insert` interleaving with B `set_user_groups` on the same user can produce a union/duplicate or a dropped set, since neither pair is atomic relative to the other.
- Consequence: transient privilege loss (a permission check in the window sees no memberships) and, under two concurrent `set_user_groups`, a diverged final set.
- Contrast: the typed `M2M::set` (`crates/umbral-core/src/orm/m2m.rs:330-419`) and `set_junction_dynamic` (`:577-637`) both correctly wrap DELETE+INSERT in `p.begin()…tx.commit()`. `set_user_groups` is the one membership path that doesn't. Fix: route it through `umbral::db::transaction(...)` (or reuse `set_junction_dynamic`). gaps ref: NEW.

**Safe:** `M2M::add` / `set` / `set_junction_dynamic` use `ON CONFLICT DO NOTHING` on the composite-PK `(parent_id, child_id)` junction (`m2m.rs:244-248, 373-377, 592-596`; DDL composite PK at `migrate.rs:545-556`). Concurrent identical adds collapse to one row. `set`/`set_junction_dynamic` DELETE+INSERT is transactional, so no empty-window for those paths.

---

## update_or_create / get_or_create

**Important (documented):** both are SELECT-then-INSERT/UPDATE with no surrounding transaction (`crates/umbral-core/src/orm/queryset/mod.rs:3886-3996`). Two missers both `create()`. The docstrings already say "pair with a UNIQUE constraint for at-most-one." With a UNIQUE constraint the loser hits the DB and returns `UniqueViolation` (not the expected `(row,false)`). Without one, two duplicate rows land. Consequence: duplicate rows when the match columns lack a UNIQUE index; otherwise an unexpected error on the race. Fix: wrap in a tx + on `UniqueViolation` re-SELECT and return `(existing,false)` to give true upsert semantics. gaps ref: NEW (or fold into the docstring's known-limitation). Real but low-severity given the documented contract.

**Important:** `update_or_create` hit-path is `first()` → `update_values()` → re-`first()` across three separate statements (`:3946-3991`). A concurrent DELETE between the UPDATE and the re-fetch yields the `"row vanished between UPDATE and re-fetch"` protocol error rather than a clean result. No corruption; a spurious error under concurrent delete. Fix: same single-transaction wrap. gaps ref: NEW.

---

## FK-existence precheck (dynamic write path)

**Safe (backstopped):** `insert_json` runs `validate_on_create` → `check_fk_row_exists` (SELECT) before the INSERT (`crates/umbral-core/src/orm/validation.rs:446-472, 556`). The SELECT→INSERT gap lets a concurrent DELETE of the parent slip through the precheck, BUT `foreign_keys=ON` + the emitted `REFERENCES` clause means the DB rejects the orphan INSERT, and `classify_or_sqlx`/`classify_sql_error` maps it to `ForeignKeyViolation`. So no orphan row is created — the precheck is an early-and-friendly error, the DB constraint is the real guard. The `_in_tx` variant additionally validates through the open tx (`:477-505`). One line so it isn't re-audited: precheck is advisory; the FK constraint is the actual invariant.

---

## Slug uniqueness

**Important:** `apply_slug_from` derives `slugify(source)` and writes it into the body (`crates/umbral-core/src/orm/write.rs:701-733`) but does **no** uniqueness disambiguation (no `-2`/`-3` suffixing, no SELECT loop). Concurrency aside, two different titles that slugify to the same string collide deterministically. If the slug column is declared `unique`, the second INSERT (concurrent or not) hits the UNIQUE constraint and surfaces as `UniqueViolation` — correct, no corruption, but no auto-suffix recovery. If the column is **not** declared `unique`, duplicate slugs silently coexist. There is no SELECT-then-pick-unique TOCTOU here because there's no SELECT at all — so it's not a classic race; it's a missing-feature + relies-on-DB-constraint. Fix: document that slug columns must be `unique`, and/or add a collision-suffix pass that catches `UniqueViolation` and retries with `-N`. gaps ref: NEW.

---

## Ambient global state (OnceLock init)

**Safe:** `POOLS`, `ATOMIC_DEFAULT`, `MODEL_ALIASES`, `PLUGIN_ORDER`, `API_ENDPOINTS`, model `REGISTRY` are all `OnceLock` set exactly once during single-threaded `App::build()` before the server accepts requests (`crates/umbral-core/src/db.rs:131-162`, `crates/umbral-core/src/migrate.rs:55-241`). `.set()` is atomic; first-writer-wins. No request thread races init because no request is served until build completes. `init()`/`init_model_aliases()` use `.expect("…called more than once")`, which would panic on a genuine double-build, but that's a programming error at boot, not a concurrent-request race. `MEM_SEQ` AtomicU64 (`db.rs:328`) is correctly atomic. No `static mut`, no lazy init reachable from two request threads.

**Safe:** signals registry is a `Mutex<Registry>` (`crates/umbral-core/src/signals.rs:144-163`) with poison-recovery via `into_inner` and per-handler `catch_unwind` (`:222`). `emit` collects futures under the lock then drops it before any `.await` (`:213-233`) — no lock-across-await deadlock. Concurrent subscribe/emit are serialised by the mutex; a subscribe during an in-flight emit either lands before or after that emit's snapshot, both consistent. The one subtlety: subscriptions registered after `App::build` (hot subscribe) are visible to subsequent emits only — fine. No divergence.

---

## Counters / aggregates

**Safe (good primitive exists):** `update_expr` emits server-side `SET col = col + 1` (`crates/umbral-core/src/orm/queryset/mod.rs:2914-2980`), the atomic path. No in-tree read-modify-write counter (`x = get(); set(x+1)`) was found in core or the audited plugins. The only read-modify-write of a column is `set_data`'s JSON blob (the Critical above). Callers must reach for `update_expr`, not `update_values(x+1)`, for counters — worth a doc callout but no current bug.

---

## Summary

Counts: 3 Critical, 6 Important, several paths confirmed safe (OnceLock init, signals mutex, M2M ON-CONFLICT + transactional set, FK precheck backstop, `update_expr` atomic counter).

Top 3 real divergence risks:
1. **`set_data` JSON-blob lost update** (`plugins/umbral-sessions/src/lib.rs:400-456`) — concurrent same-cookie writes silently drop session keys. Read-modify-write with no atomicity. Highest blast radius: every app using session `data` (cart, flash, CSRF state).
2. **`set_user_groups` non-transactional DELETE+INSERT** (`plugins/umbral-permissions/src/membership.rs:77-97`) — opens a window where a concurrent permission check sees zero memberships (transient privilege loss), and concurrent calls diverge the final set. Every sibling junction-replace path is already transactional; this one regressed.
3. **`update_or_create` / `get_or_create` without a tx** (`crates/umbral-core/src/orm/queryset/mod.rs:3886-3996`) - duplicate rows when match columns lack a UNIQUE constraint; an unexpected `UniqueViolation` error (instead of `(row,false)`) when they have one. Documented as a known limitation but still a correctness gap for the upsert contract.

Secondary fixes worth bundling: catch `UniqueViolation`→`Ok(())` in `add_user_to_group`/`grant_user_permission` (idempotency contract), and slug collision handling / require-unique doc.
