---
name: dynqueryset-update-return-semantics
description: Use when you need to know whether an ORM UPDATE actually matched a row (ownership checks, 404-on-missing, optimistic concurrency). DynQuerySet::update_json / update_json_in_tx do NOT return an affected-row count.
---

# DynQuerySet update return value is not an affected-row count

## Context

You have a `DynQuerySet::for_meta(m).filter_eq_string(pk, id).update_json_in_tx(body, tx)` and you want to know "did that WHERE actually match a row?" — e.g. to return 404 when the id doesn't exist, to enforce that a nested child belongs to a given parent, or for optimistic-concurrency. The obvious move is `if affected == 0 { return NotFound }`. **That check silently never fires.**

## Approach

The return value of `update_json_in_tx` (and `update_json`) is:

```rust
Ok(parent_pks.len().max(if any { 1 } else { 0 }) as u64)
```

where `any` = "the body supplied at least one settable column" and `parent_pks` = the PKs the WHERE matched. So when the body has any SET columns, the result is `max(0, 1) == 1` **even if the WHERE matched zero rows**. It is a "did we build a non-empty UPDATE" signal, not `ROWS AFFECTED`. Only when the body is empty (no SET columns, no M2M) does it return 0.

Therefore, to test whether a row exists / is owned, do an **explicit existence read first**, then update:

```rust
let owned = DynQuerySet::for_meta(&child)
    .filter_eq_string(&child_pk, &pk_str)
    .filter_eq_string(&fk, parent_id)     // scope to the owning parent
    .fetch_one_json_in_tx(&mut tx)         // -> Result<Option<Map>, _>
    .await?;
if owned.is_none() {
    return Err(ApiError::NotFound(/* ... */));
}
// ownership confirmed — now apply the partial update
DynQuerySet::for_meta(&child)
    .filter_eq_string(&child_pk, &pk_str)
    .update_json_in_tx(&body, &mut tx)
    .await?;
```

`fetch_one_json_in_tx` returns a real `Option<Map>` — `None` genuinely means "no matching row" — so it's the right primitive for the gate.

## Why

`update_json_in_tx` reads `parent_pks` via `collect_parent_pks_in_tx` primarily to mirror M2M junction rows, not to report affected rows; the `.max(1)` exists so a scalar-only update on a matched row reports success. The count was never intended as a caller-facing "rows changed" contract, and the flat `update_json` shares the shape. Changing the return type to a true affected count would be a cross-cutting ORM change (a deferred-spec item); until then, callers must not lean on it.

## Pitfalls

- The bug is invisible without a test that PATCHes a **non-existent / cross-owner id** and asserts 404 — a happy-path update test passes either way. This is exactly the case a behavioral test (real rows, cross-parent id, read back) catches and an assert-the-SQL test misses.
- `flat update_json` has the same semantics — same fix.
- Don't "fix" it by making the child update filter on both pk AND fk and trusting the count; the count still lies. The existence read is the gate; the fk filter on it is what scopes ownership.

## See also

- `plugins/umbral-rest/src/lib.rs` `update_nested` (gaps3 #9) — the nested-update ownership gate that this skill was extracted from.
- `crates/umbral-core/src/orm/dynamic.rs` `update_json_in_tx` / `fetch_one_json_in_tx`.
- Memory: [Behavioral tests, not random asserts] — why the cross-parent case must be exercised, not asserted structurally.
