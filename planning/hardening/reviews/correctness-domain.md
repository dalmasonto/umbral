done hardening

# Correctness & Domain-Parity Review — umbral

Scope: read-only correctness / spec-parity audit of the ORM, migration engine, dynamic (admin/REST) path, and validators. Severities: **Critical** (data corruption / silent wrong result / unsafe migration), **Important** (behavior contradicts docs/spec on a common path), **Optional**, **FYI**. `file:line` cited from the working tree at review time. Existing gap numbers cited where they match; otherwise **NEW**.

---

## Soft-delete

The typed read path is sound: `build_query_for` (`crates/umbral-core/src/orm/queryset/mod.rs:489-495`) injects `WHERE deleted_at IS NULL` for every read terminal (`fetch`/`first`/`count`/`aggregate`/`exists` all route through it), `delete()` rewrites to `UPDATE ... SET deleted_at = NOW()` (`mod.rs:2807-2813`, `4575-4585`), annotate-counts fold the child filter (`mod.rs:551`), and the `search` scope honors it (`orm/search.rs:141-142`). The leaks are concentrated on the **write**, **dynamic**, and **relation-hydration** surfaces — and all three are untested (`tests/soft_delete.rs` covers only `delete()`-rewrite and `hard_delete()`-bypass).

**Critical:** Dynamic path hard-deletes and lists trashed rows on `#[umbral(soft_delete)]` models (`crates/umbral-core/src/orm/dynamic.rs:584-619`, plus the whole `DynQuerySet`) — expected: admin/REST scope to `deleted_at IS NULL` for reads and soft-delete (UPDATE) on delete, like the typed path; actual: `DynQuerySet::for_meta` (`dynamic.rs:139`) never reads `meta.soft_delete` (which IS correctly populated at `migrate.rs:411`), so `count()`/`fetch_*`/`delete()` ignore it — `delete()` issues a raw `DELETE FROM` (`dynamic.rs:595-611`) that **permanently destroys** the row the typed path would only soft-delete, and changelists/REST lists return trashed rows. → Fix: inject `deleted_at IS NULL` into every `DynQuerySet` terminal when `meta.soft_delete` (honoring an `only_deleted`/`with_deleted` toggle), and rewrite `DynQuerySet::delete()` to UPDATE like the typed path. gaps2 **#35** (confirmed, exact).

**Critical:** `update_values` / `update_expr` mutate soft-deleted rows (`crates/umbral-core/src/orm/queryset/mod.rs:3233-3235` in `build_update_for`, and `mod.rs:2937-2939` in `update_expr`) — expected: a bulk update on a soft-delete model should not touch trashed rows (or should honor `only_deleted`/`with_deleted`); actual: both walk **only** `self.predicates` and never add the `deleted_at IS NULL` guard, so `Post::objects().update_values(...)` silently mutates trashed rows and `.only_deleted()`/`.with_deleted()` are silent no-ops on the update path. Note: gaps2 #34 cites `~2403`; the real sites are `build_update_for` at `mod.rs:3205-3236` **and** `update_expr` at `mod.rs:2914-2939` (the latter is not mentioned in #34 and has the identical hole). → Fix: inject the soft-delete guard into both update builders, honoring the visibility toggles. gaps2 **#34** (confirmed; extend to cover `update_expr`).

**Important:** Prefetch / reverse-FK / M2M hydration returns trashed children (`crates/umbral-core/src/orm/queryset/hydration.rs:654-694` and the reverse-FK builder at `:356`; zero soft-delete refs in `hydration.rs`, `reverse_set.rs`, `m2m.rs`, `one_to_one.rs`) — expected: `parent.prefetch_related("comments")` should match `Comment::objects().filter(...).fetch()`, which excludes trashed rows; actual: the child query is a raw `sea_query::Query::select()` against `child_meta.table` (which carries `soft_delete`) with no `deleted_at IS NULL` clause, so a soft-deleted child appears in the prefetched set while it is invisible via its own manager. This is a **third surface** not covered by #34 or #35. → Fix: when `child_meta.soft_delete`, add `AND <child>.deleted_at IS NULL` to the prefetch/reverse/M2M child SELECTs (mirror the annotate-count fold at `mod.rs:551`). **NEW.**

**Optional:** FK-existence validation does not honor soft-delete (`crates/umbral-core/src/orm/validation.rs:566-571`) — a dynamic-path FK write can point at a soft-deleted target because the existence `count()` on `DynQuerySet` (per the Critical above) sees trashed rows as live. Resolved automatically once #35 is fixed; flagged so the FK-validation test covers it. **NEW** (depends on #35).

---

## PK-types (partial PK-lift refactor)

Hydration is well-lifted (select_related, prefetch, reverse-O2O, `in_bulk`, `get_or_create`/`update_or_create`, M2M instance CRUD all key via `pk_as_json()`/`pk_key()`). The gaps are on the form-write and a couple of dynamic-coercion paths.

**Critical:** `validate_multi_fk_exists` drops String/Uuid M2M child ids from the staged junction write (`crates/umbral-core/src/orm/forms_runtime.rs:224-232`, the `id.parse::<i64>()` at `:226`) — expected: every submitted child id that exists is staged into the junction; actual: existence is checked PK-agnostically and confirmed in `found`, but the return loop only pushes `Value::BigInt` when `parse::<i64>()` succeeds, so a String/Uuid child id is **validated-as-present then silently dropped** — the junction row is never written. Silent M2M data loss on a form write whose child model has a non-i64 PK. No test exercises a non-i64 **child** PK through this function. → Fix: dispatch on the target PK `SqlType` (the fn already has `meta`/`pk_col`) and emit `String`/`Uuid`/`BigInt` like `filter_in_strings` (`dynamic.rs:414`). **NEW** (sibling of the gap #112 FK-target lift that missed this fn; adjacent to gaps2 #48).

**Important:** Instance reverse accessor `parent.reverse::<C>()` errors for String/Uuid parents (`crates/umbral-core/src/orm/reverse_accessor.rs:171-173`, `self.pk_i64().ok_or(NonI64Pk)`) — expected: works for any PK type (the declared `ReverseSet`+`prefetch` path was lifted and `tests/pk_string_reverse_fk.rs` passes for a slug PK); actual: the zero-declaration instance accessor still reads `pk_i64()` and returns `NonI64Pk` for any non-i64 parent. Fails loudly (not corruption), but it is an inconsistency with the lifted declared path. → Fix: read `pk_as_json()` + the parent PK `SqlType` and build the predicate via `json_to_sea_value`, as `hydrate_reverse_fk_for_field` (`hydration.rs:356`) already does. Was gaps2 #45 (archived as a v1 constraint); **NEW** as an open follow-up now that the rest of the refactor moved past it.

**Optional:** `DynQuerySet::create` returns `0` for a present-but-non-i64 PK (`crates/umbral-core/src/orm/dynamic.rs:824`, `row.try_get::<i64,_>(...).unwrap_or(0)`) — a String/Uuid PK fails the i64 decode and silently yields `0`; any caller trusting the returned id (redirect-to-new-row, audit log) gets `0`. → Fix: return the PK as JSON/string, branching on PK type like the read path at `dynamic.rs:1521`. Tied to the PK-lift work (MEMORY: project_primary_key_refactor). **NEW.**

**FYI:** CSV-import FK coercion (`dynamic.rs:2874-2877`) parses FK cells to i64 then falls back to raw string — works for Text/Uuid targets only by accident, unlike the explicit dispatch at `dynamic.rs:2000-2018`. Stale doc comment at `hydration.rs:536-537` still says "i64 parent PK only" on a now-PK-agnostic body. Admin free-text `?q=` builds an i64-eq predicate for FK columns (`dynamic.rs:266-268`) — a search miss for non-numeric FK values, not data loss.

---

## Error-paths (silent swallowing — CLAUDE.md "fix don't patch")

**Important:** Malformed inline-edit POST body writes empty string into the cell and returns 200 (`plugins/umbral-admin/src/handlers/inline_edit.rs:163-167`) — `serde_urlencoded::from_str(&body).unwrap_or_default()` turns a urldecode failure into an empty map, `new_value` becomes `""`, `update_one(&field, "")` blanks the cell, and the handler returns `200 OK` with the blanked value rendered back; the user sees the edit "succeed" while the prior value is lost. Sibling handlers (`sheet.rs:385`, `actions.rs:44`) guard this; inline_edit is the outlier. → Fix: `match from_str { Ok(m)=>m, Err(e)=> return BadInput.into_response() }` like `actions.rs:44`. **NEW** (gaps2 #50 covers child inline-edit, not this scalar cell path).

**Important:** FK-existence check turns a DB error into a validation rejection (`crates/umbral-core/src/orm/validation.rs:566-571`, `.count().await.unwrap_or(0)`) — a failed COUNT (broken pool, missing table) makes `check_fk_row_exists` report "FK target does not exist", so a transient DB error surfaces as a bogus FK-validation error instead of a 500. Fails closed (no corruption) but masks the real fault. → Fix: return `Result<bool, DynError>` and `?`-propagate. **NEW.**

**Optional:** `Messages::drain` clears the flash queue even when the read errored (`plugins/umbral-sessions/src/lib.rs:708-710`) — `read(...).unwrap_or_default()` returns empty on a DB error, then the queue is cleared unconditionally, destroying pending messages the user never saw. → Fix: clear only on a successful read. **NEW.** | Sidebar/list model counts swallow DB errors as `0` (`plugins/umbral-admin/src/handlers/list.rs:326`; `DynQuerySet::create` PK `0` already listed above) — a broken table renders "0" instead of surfacing the fault. **NEW.**

**FYI:** `get_data`/`set_data` (`plugins/umbral-sessions/src/lib.rs:389,447`) swallow malformed JSON to an empty map while the docstring claims it errors — framework-owned column, doc/behavior mismatch only. The `object_id.parse::<i64>().ok()` audit-log sites (admin `crud.rs:563`, `inline_edit.rs:172`, `sheet.rs:431`) log `None` for non-i64 PKs (revisit under the PK-lift). Numerous other `unwrap_or_default`/`let _ =` sites reviewed and judged legitimate (NULL→display decoding, in-memory CSV writers, OnceLock-set idioms, tx-rollback error paths, CSRF fail-safe defaults).

---

## Migrations (autodetector)

The cross-plugin / in-batch FK CreateTable ordering is a sound Kahn topo-sort (`crates/umbral-core/src/migrate.rs:2306-2367`), the AddColumn NOT-NULL-without-default guard is correct (`migrate.rs:2784-2807`), and column-rename detection exists (single-drop+single-add + `column_shape_matches`, `migrate.rs:2738-2759`). The holes are unsafe column *alters* on populated tables, M2M-rename data loss, and the absent data-migration / squash machinery — and the unsafe alters have **no test** (`tests/rename_detection.rs` covers table rename only; `tests/migration_safety.rs:82` tests the AddColumn warning only).

**Critical:** nullable → NOT NULL tightening emits a bare `SET NOT NULL` with no NULL pre-check or backfill (`crates/umbral-core/src/migrate.rs:2673-2685` diff → `:3375-3384` PG render, `:3489-3500` SQLite rebuild) — expected: refuse without a default, or emit a backfill `UPDATE ... WHERE col IS NULL` first (the code even documents the hazard at `:3330`); actual: unconditional `AlterColumn` → raw `ALTER COLUMN ... SET NOT NULL` that aborts mid-migration on any existing NULL row. Only a soft `OpSafety::Warning` (`:2086`) flags it. → Fix: in `diff_columns`, when `prev.nullable && !curr.nullable`, refuse with `UnsafeAlter` unless a default exists (mirror the gap-97 AddColumn guard) or emit a backfill. **NEW** (gap-97 family, AlterColumn variant).

**Critical:** unique false → true emits a bare `ADD CONSTRAINT ... UNIQUE` with no duplicate pre-check (`migrate.rs:2676` diff → `:3394-3399` PG render; SQLite rebuilds with the UNIQUE column) — exactly the "UNIQUE addition trips an existing duplicate" hazard CLAUDE.md names; treated as an ordinary `AlterColumn` and aborts at apply time on a table with dup values. → Fix: classify a false→true unique flip as its own unsafe tier with a duplicate-checking message. **NEW.**

**Important:** M2M junction is dropped+recreated on a parent-model rename, destroying all relationship rows (`migrate.rs:2378-2408`; `collect_m2m_pairs` keys on `(table, field)` at `:2439`) — Pass 0/1 correctly emit `RenameTable` for the base table, but the renamed parent table yields a different M2M key, so the old junction is seen as removed (`DropM2MTable`) and the new as added (`CreateM2MTable`); the base rename "succeeds" while junction rows silently vanish. The code comment at `:2382-2385` acknowledges it. → Fix: pair junction renames off the same table-rename detection (`{from}_{field}` → `{to}_{field}`). gaps **#93** (confirmed).

**Important:** No data-migration / backfill escape hatch (`migrate.rs:480-590` Operation enum has no `RunSql`/`RunPython`; `:2784-2807`) — backfilling a new column *from* existing data is impossible; the only backfill is a literal constant default, and `operations`+`snapshot_after` are coupled at `:977` so there's no state-only/schema-only split. → Fix: add a `RunSql { up, down }` Operation variant. gaps **#89/#90** (confirmed). | No `squashmigrations` command anywhere — gaps **#94** (confirmed).

**Important (test gap):** column-rename detection (`migrate.rs:2738-2759`) auto-pairs a single drop+add unconditionally when shapes match, so two genuinely-different same-shape columns become a RENAME (wrong data preserved) on an `eprintln!`-only warning; **no test** exercises `diff_columns` rename at all. → Fix: add behavioral tests for the rename path + the >1 fallback. gap **#88** shipped the op; the test gap is **NEW.**

**FYI:** A true FK cycle within one diff batch falls through to declaration order (`migrate.rs:2334-2346`) and relies on the apply-time DB error — acceptable (surfaces a real error).

---

## Parity (expected ORM semantics)

`update_or_create`/`get_or_create` (`mod.rs:3886-3996`), reverse-O2O (`one_to_one.rs:177-222`), NULL handling (`write.rs:408-423` → `RequiredFieldMissing`, never a panic), empty-set aggregates (SUM/MAX/MIN → NULL not 0, COUNT → 0; `backend_sqlite.rs:258-286`), and choices-validation timing all **match the expected semantics** and are correct.

**Important:** FK `on_delete` is DDL-only - no ORM-level collector (`crates/umbral-core/src/orm/foreign_key.rs:62`; `delete()` is a plain `DELETE FROM` at `mod.rs:2807-2890`) - expected: `on_delete` enforced by an ORM-level collector independent of DB FK actions, firing `post_delete` per cascaded row, working even with SQLite FKs off; actual: cascade/set-null/restrict happen only via the DDL `ON DELETE` clause. umbral does enable SQLite FK enforcement on the managed pool (`db.rs:350`), but (a) cascaded child deletes fire **no** `post_delete`/`bulk_post_delete` signals (the gap-#77 audit log silently misses cascaded rows), (b) any SQLite connection opened without the pragma leaves orphans, (c) RESTRICT/PROTECT surfaces as a raw sqlx FK error, not a friendly `ProtectedError`. → Fix: add an ORM collector in `delete()` that walks reverse FKs by action and fires per-row signals; keep DDL as the backstop. gap **#68** deferred the enforcement layer - **NEW** as the open follow-up.

**Important:** default `on_delete` is `NoAction` rather than a required choice (`crates/umbral-core/src/orm/model.rs:785-790`, `:622-624`) - the developer should be forced to pick CASCADE/PROTECT/SET_NULL; umbral silently defaults to `NO ACTION`, which (with enforcement on) behaves like PROTECT and is a declaration-time footgun. → Fix: make `on_delete` a required derive attribute for non-nullable FKs, or document the default prominently. **NEW.**

**Optional:** `bulk_create` returns `u64`, discarding inserted rows with populated PKs (`crates/umbral-core/src/orm/queryset/mod.rs:3752`; body already does `RETURNING <pk>` at `:3784-3829` but keeps only the count for the signal) - the expectation is to return the objects with PKs populated. → Fix: return `Vec<T>` (or add `bulk_create_returning`) from the rows it already fetches. **NEW.** (Note: umbral goes *further* by validating choices/FK on `bulk_create`, which is a deliberate improvement, not a bug.)

---

## Validation

All length and numeric **boundary operators are correct - no off-by-one found**: MinLength `< n` (`forms.rs:208`), MaxLength `> n` (`forms.rs:223`), numeric min `< min` / max `> max` (`dynamic.rs:1350/1358` and `:2658/2666`) - all use inclusive bounds, and lengths count chars not bytes (multibyte-safe). The choices membership check (`validation.rs:328`), `MultiChoice::from_csv` (`multichoice.rs:113-129`), and the phone/regex/integer validators are solid. **No boundary case is directly tested**, though, so a future `<`/`<=` edit would not be caught.

**Important:** numeric `min`/`max` silently skipped for float/decimal values (`crates/umbral-core/src/orm/dynamic.rs:1348` and `:2656`, gated on `json.as_i64()`) — `min`/`max` are `Option<i64>` but expressible on a `Real`/`Double` column; at write time the range check only runs when `as_i64()` succeeds, so any fractional value (including out-of-range `-3.5`, `99.9`, and even `7.0` which serde reads as f64) **bypasses the check entirely and is written**. Same hole in both insert (`:2656`) and update (`:1348`) paths; the range validator has **no test at all**. → Fix: fall through to `json.as_f64()` and compare against `min as f64`/`max as f64`, or add a system check rejecting min/max on non-integer field types. **NEW.**

**Optional:** numeric choices effectively String-only (`crates/umbral-core/src/orm/validation.rs:321-327`) — a JSON `2.0` stringifies to `"2.0"`, never matching choice `"2"`, producing a spurious "not a valid choice". → Fix: normalize integer-valued floats before stringify. **NEW.** | Duplicate email/url validators disagree across layers: form-layer `EmailField`/`URL_PATTERN` (`forms.rs:238,320`) accept inputs (`"a b@example.com"`, `https://localhost`) that the ORM wrapper-type `validate_email_str`/`validate_url_str` (`validators.rs:357,376`) reject. → Fix: route both through one source of truth. **NEW.**

**FYI:** `Required` (form) rejects whitespace-only via `trim()` but the ORM's `value_is_blank_for_type` (`validation.rs:520-538`) treats `"   "` as non-blank on a Text column — inconsistent but not a security issue. Slug validator is correctly tight (ASCII-only, `validators.rs:341-345`). `Field::validate` empty-skip uses `is_empty()` not `trim().is_empty()` (`forms.rs:672`).

---

## Summary

**Counts:** Critical 5 · Important 9 · Optional 7 · FYI (grouped) across all areas.

- Critical: soft-delete dynamic path (gaps2 #35), soft-delete `update_values`/`update_expr` (gaps2 #34), M2M-child non-i64 PK form drop (NEW), migration nullable→NOT NULL on populated tables (NEW), migration unique-add on duplicate values (NEW).
- Important: prefetch returns trashed children (NEW), inline-edit blanks cell on bad body (NEW), FK-check masks DB error (NEW), instance reverse-accessor non-i64 (NEW), M2M junction rename data loss (#93), no data-migration hatch (#89/#90), no squash (#94), on_delete not ORM-enforced (#68 follow-up), float min/max bypass (NEW).

**Top 3 correctness risks:**

1. **Soft-delete dynamic path (Critical, gaps2 #35).** Every admin "Delete" button and REST `DELETE` on a `#[umbral(soft_delete)]` model is a permanent hard-delete, and changelists/REST lists show trashed rows — the meta carries `soft_delete` correctly (`migrate.rs:411`) but `DynQuerySet` ignores it entirely. Highest blast radius: the website tagged 23 models.
2. **Unsafe column ALTERs on populated tables (Critical, NEW).** `nullable→NOT NULL` and `unique false→true` both emit bare ALTER/ADD-CONSTRAINT with no NULL/duplicate pre-check, aborting mid-migration on real data — directly the failure modes CLAUDE.md says the engine exists to expose, but it lets them through with only an advisory warning, untested.
3. **Silent M2M / numeric data integrity (Critical/Important, NEW).** Non-i64 M2M child ids silently dropped from form-driven junction writes (`forms_runtime.rs:226`), and float values bypass `min`/`max` validation entirely (`dynamic.rs:1348/2656`) — both write wrong/incomplete data while reporting success, and both are untested.
