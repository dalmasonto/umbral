# Model-level audit trail — `#[umbral(audited)]` (gaps3 #54 / kikosi #7)

Status: **designed, not built.** Author: 2026-07-12.

## What it is, and what it is not

Per-model change history: *who* changed *which row*, *when*, and *from what to what*. Opt-in per model, recorded for **every** write path — code, REST, admin, tasks.

**It is not `AdminAuditLog`.** `plugins/umbral-admin/src/models.rs:357` already exists and is easy to mistake for this. It is Django's `LogEntry`: it records only writes made *through the admin UI*, its `diff_summary` is free-text prose rather than a field-level before/after, and a write from REST, a task, or `Model::objects().save()` produces **no row at all**. Anyone reading it as an audit trail is being misled.

## Verified ground truth

Read from the code, not assumed:

1. **The `RouteContext.user` plumbing already exists** (`crates/umbral-core/src/db/route_context.rs`, `current_user_id()`), shipped with gaps3 #55. The "who" is solved — an audit row can read the authenticated caller ambiently, and it is `None` (not a guess) for jobs/CLI/anonymous.
2. **Signals are NOT a sufficient hook — this is the trap.** `emit_post_save` on the typed path carries the **full serialized instance** (`signals.rs:347`). But `emit_bulk_post_save_by_table` on the dynamic path carries only `{"ids": [...], "created": bool}` (`signals.rs:442`) — **no row data**. Admin and REST run entirely on `DynQuerySet`. So an audit trail built on signals would give rich history for code-driven writes and PK-only stubs for the writes humans actually make. That asymmetry is worse than no audit trail, because the log *looks* complete.
3. **There is no pre-image.** Nothing in the ORM reads the row before writing it (`grep old_row|before_row|previous` over `orm/write.rs`: zero). A before/after diff therefore costs one extra `SELECT` per audited update/delete. That is the price of the feature and should be paid only for `audited` models.
4. **There is no single write choke point** — the same structural fact that made the soft-delete cascade a two-path change. Typed: `Manager::create`, `bulk_create`, `update_values`/`update_expr`, `delete`. Dynamic: `insert_json`(`_in_tx`), `update_json`(`_in_tx`), `update_form`, `delete`, `restore`. Hooking only the typed path leaves admin and REST unaudited **with a green test suite**.

## Design

### The record

A framework-owned model in `umbral-core`, registered automatically when any model is `audited` (so `makemigrations` creates its table through the normal declare→migrate loop — no special-cased DDL):

```rust
#[derive(Model)]
#[umbral(table = "umbral_audit")]
struct AuditEntry {
    id: i64,
    table_name: String,          // which model
    row_pk: String,              // PK-shape independent (i64 / String / Uuid all stringify)
    action: String,              // "create" | "update" | "delete"
    actor: Option<String>,       // current_user_id() — None for job/CLI/anonymous, never a guess
    at: DateTime<Utc>,
    changes: String,             // JSON: {"field": {"from": .., "to": ..}, ...}
}
```

`row_pk` and `actor` are strings deliberately: the user model and the audited model may have any PK shape, and the audit table must not care.

### Capture

- **create** — `changes` is `{field: {from: null, to: v}}` for the inserted row. No pre-image needed.
- **update** — SELECT the affected rows first (the pre-image), apply the write, then emit one entry per row with only the fields that actually **changed**. Recording untouched fields makes the log unreadable and enormous.
- **delete** — the pre-image is the record. Soft delete is an update (`deleted_at`), so it audits as `delete` rather than `update`, or the log will not say what a human means by "deleted".

### The `audited` flag

`#[umbral(audited)]` → `Model::AUDITED` → **`ModelMeta.audited`** — on `ModelMeta`, so the dynamic path can read it. Mirrors `soft_delete` exactly (`migrate.rs:386`), and NOT the mistake of re-deriving it per-path.

## Why this is not a small change

Three multipliers, each verified above: no single choke point (both write paths, ~9 sites), no pre-image (a new SELECT on the audited update/delete paths), and a new framework-owned table that must register itself into the migration graph.

**An audit trail that silently misses writes is worse than none**, because it is the one log you would testify from. That is the reason to build it properly or not at all — the same argument that killed row-level tenancy, applied to a feature that genuinely *should* be built.

## Sequencing

1. Flag: macro → `Model::AUDITED` → `ModelMeta.audited`.
2. `AuditEntry` model + auto-registration when any model is audited.
3. `orm/audit.rs`: `record(table, pk, action, before, after)` + the changed-fields diff.
4. Typed path hooks (create / bulk_create / update / delete), each with the pre-image read.
5. **Dynamic path hooks** — the one that must not be skipped; admin and REST live here.
6. Admin: a read-only `AuditEntry` model, plus a per-object history tab.
7. Tests: every write path (typed, REST, admin, task) produces exactly one entry with the right actor and the right changed-fields — and a write with no caller records `actor = NULL` rather than inventing one.
