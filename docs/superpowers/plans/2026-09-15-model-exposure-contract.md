# Model Exposure Contract Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `ModelMeta` a small set of derived-view methods so every serializer plugin (REST, GraphQL, admin, future gRPC) reads a model's serializable/writable field sets, display field, and relations from ONE shared place instead of re-deriving them.

**Architecture:** Inherent methods on the existing `ModelMeta` struct, added in a new module `crates/umbral-core/src/orm/exposure.rs` kept strictly separate from the migration-snapshot code in `migrate.rs`. Methods return declarative *facts*; each plugin layers its own transport *policy* on top. No new trait (extract one later only if a second implementor appears).

**Tech Stack:** Rust, `umbral-core` (ORM), `#[derive(Model)]` from `umbral-macros`, existing `ModelMeta`/`Column` types in `crates/umbral-core/src/migrate.rs`.

**Spec:** `docs/decisions/2026-09-15-model-exposure-contract.md` (read it — the plan argues from it).

## Global Constraints

- `umbral-core` must NOT depend on any plugin. This work is entirely inside `umbral-core`; it names no plugin. (CLAUDE.md dependency inversion.)
- The exposure methods live ONLY in `crates/umbral-core/src/orm/exposure.rs`, never in `migrate.rs`. Schema-snapshot concerns and runtime-exposure concerns stay in separate files even though both `impl ModelMeta`.
- Add only methods that do real derivation. `ModelMeta`'s fields are all `pub` (`name`, `table`, `ordering`, `search_fields`, `list_display`, `list_filter`, `readonly_fields`, `inline_edit_fields`, `m2m_relations`, `fields`, `display`, `str_template`) and are read directly; do NOT add pass-through accessors for them. `table_name()` already exists on `ModelMeta` — do not redefine it.
- Behavioural tests, not SQL/string assertions: define real models, read derivations back, assert the field sets (per the repo's testing convention).
- Before every commit: `cargo fmt`, `cargo clippy --all-targets`, `cargo build`, `cargo test` for the touched crate must pass. Never `--no-verify`.
- Commit attribution footer on every commit: `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.

---

### Task 1: The exposure module — derived-view methods + behavioural tests

**Files:**
- Create: `crates/umbral-core/src/orm/exposure.rs`
- Modify: `crates/umbral-core/src/orm/mod.rs` (add `pub mod exposure;` among the other `pub mod` lines, ~line 22-40)
- Test: `crates/umbral-core/tests/model_exposure.rs`

**Interfaces:**
- Consumes: `crate::migrate::{ModelMeta, Column}` (existing); `ModelMeta::for_::<T>() -> ModelMeta` (existing, `crates/umbral-core/src/migrate.rs:565`); `Column` public flags `primary_key`, `secret`, `private`, `noform`, `privileged`, `is_string_repr`, `auto_now`, `auto_now_add`, `auto_user`, `auto_user_add`, `auto_uuid`, `fk_target`, `name` (existing, `migrate.rs:1030+`).
- Produces (these are what later tasks and every plugin rely on — exact signatures):
  - `ModelMeta::field(&self, name: &str) -> Option<&Column>`
  - `ModelMeta::display_field(&self) -> Option<&Column>`
  - `ModelMeta::public_fields(&self) -> impl Iterator<Item = &Column>`
  - `ModelMeta::serializable_fields(&self, allow_private: bool) -> impl Iterator<Item = &Column>`
  - `ModelMeta::is_server_managed(&self, f: &Column) -> bool`
  - `ModelMeta::writable_fields(&self) -> impl Iterator<Item = &Column>`
  - `ModelMeta::foreign_keys(&self) -> impl Iterator<Item = &Column>`

- [ ] **Step 1: Write the failing test**

Create `crates/umbral-core/tests/model_exposure.rs`:

```rust
//! The model-exposure contract: `ModelMeta`'s derived views over a model's
//! declared field facts. Behavioural — real models via `ModelMeta::for_::<T>()`
//! (the same value the runtime registry caches), asserting the resulting field
//! SETS, not generated SQL. See docs/decisions/2026-09-15-model-exposure-contract.md.
#![allow(dead_code)]

use chrono::{DateTime, Utc};
use umbral::migrate::ModelMeta;
use umbral::orm::{ForeignKey, Model};

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]
#[umbral(table = "mx_category")]
pub struct Category {
    pub id: i64,
    #[umbral(string)]
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]
#[umbral(table = "mx_product")]
pub struct Product {
    pub id: i64, // primary key -> server-managed, never client-writable
    #[umbral(string)] // is_string_repr -> the display field
    pub name: String,
    #[umbral(private)] // confidential read; still WRITABLE (private is a read fact)
    pub cost: String,
    #[umbral(secret)] // never serialized; still writable (secret is a read fact)
    pub signing_key: String,
    #[umbral(privileged, default = "false")] // mass-assignment guarded -> not writable
    pub is_featured: bool,
    #[umbral(auto_now_add)] // server-populated -> not writable
    pub created_at: DateTime<Utc>,
    pub category: ForeignKey<Category>, // foreign key
}

fn names<'a>(it: impl Iterator<Item = &'a umbral::migrate::Column>) -> Vec<String> {
    it.map(|c| c.name.clone()).collect()
}

#[test]
fn field_lookup_finds_by_name_and_misses_cleanly() {
    let meta = ModelMeta::for_::<Product>();
    assert_eq!(meta.field("cost").map(|c| c.name.as_str()), Some("cost"));
    assert!(meta.field("does_not_exist").is_none());
}

#[test]
fn display_field_is_the_is_string_repr_column() {
    let meta = ModelMeta::for_::<Product>();
    assert_eq!(meta.display_field().map(|c| c.name.as_str()), Some("name"));
}

#[test]
fn public_fields_exclude_private_and_secret() {
    let meta = ModelMeta::for_::<Product>();
    let public = names(meta.public_fields());
    assert!(public.contains(&"name".to_string()));
    assert!(public.contains(&"id".to_string()));
    assert!(
        !public.contains(&"cost".to_string()),
        "private field must be excluded"
    );
    assert!(
        !public.contains(&"signing_key".to_string()),
        "secret field must be excluded"
    );
}

#[test]
fn serializable_fields_adds_back_private_when_allowed_never_secret() {
    let meta = ModelMeta::for_::<Product>();

    let locked = names(meta.serializable_fields(false));
    assert_eq!(locked, names(meta.public_fields()), "false == public set");

    let unlocked = names(meta.serializable_fields(true));
    assert!(
        unlocked.contains(&"cost".to_string()),
        "private added back when allowed"
    );
    assert!(
        !unlocked.contains(&"signing_key".to_string()),
        "secret is NEVER serialized, unlock or not"
    );
}

#[test]
fn is_server_managed_covers_pk_and_auto_columns() {
    let meta = ModelMeta::for_::<Product>();
    let by = |n: &str| meta.field(n).map(|c| meta.is_server_managed(c)).unwrap();
    assert!(by("id"), "primary key is server-managed");
    assert!(by("created_at"), "auto_now_add is server-managed");
    assert!(!by("name"), "a plain field is not server-managed");
}

#[test]
fn writable_fields_exclude_pk_auto_and_privileged_but_keep_private_and_secret() {
    let meta = ModelMeta::for_::<Product>();
    let writable = names(meta.writable_fields());
    assert!(writable.contains(&"name".to_string()));
    // private/secret are READ facts; the fields stay writable (write-blocking
    // is noform/privileged, which these are not):
    assert!(writable.contains(&"cost".to_string()));
    assert!(writable.contains(&"signing_key".to_string()));
    assert!(!writable.contains(&"id".to_string()), "pk excluded");
    assert!(
        !writable.contains(&"created_at".to_string()),
        "auto_now_add excluded"
    );
    assert!(
        !writable.contains(&"is_featured".to_string()),
        "privileged excluded"
    );
}

#[test]
fn foreign_keys_returns_only_fk_columns_with_their_target() {
    let meta = ModelMeta::for_::<Product>();
    let fks: Vec<_> = meta.foreign_keys().collect();
    assert_eq!(fks.len(), 1, "one FK on Product");
    assert_eq!(fks[0].fk_target.as_deref(), Some("mx_category"));
    // A model with no FK returns an empty iterator.
    assert_eq!(ModelMeta::for_::<Category>().foreign_keys().count(), 0);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p umbral-core --test model_exposure`
Expected: FAIL — compile errors, "no method named `field`/`public_fields`/… found for struct `ModelMeta`".

- [ ] **Step 3: Register the module**

In `crates/umbral-core/src/orm/mod.rs`, add the line among the existing `pub mod` declarations (keep alphabetical position near `pub mod dynamic;`):

```rust
pub mod exposure;
```

- [ ] **Step 4: Write the module**

Create `crates/umbral-core/src/orm/exposure.rs`:

```rust
//! The model-exposure contract: the derived views over `ModelMeta` that every
//! serializer plugin (REST, GraphQL, admin, a future gRPC) reads to learn a
//! model's shape, instead of re-deriving "which fields are serializable /
//! writable" by hand.
//!
//! Kept separate from the migration-snapshot code in `migrate.rs`: this module
//! owns runtime exposure, that one owns schema snapshots — even though both
//! `impl ModelMeta`.
//!
//! Facts on the model, policy in the plugin: these methods return declarative
//! facts; each plugin layers its own transport policy (REST `.hide()`,
//! permissions, nesting) on top. Raw per-field facts (`help`, `widget`,
//! `choices`, `secret`, `private`, …) and the declared presentation lists
//! (`list_display`, `search_fields`, …) are read directly off the public
//! `Column`/`ModelMeta` fields — this module adds only DERIVED views.
//!
//! See `docs/decisions/2026-09-15-model-exposure-contract.md`.

use crate::migrate::{Column, ModelMeta};

impl ModelMeta {
    /// Look up a field by name.
    pub fn field(&self, name: &str) -> Option<&Column> {
        self.fields.iter().find(|c| c.name == name)
    }

    /// The display column — the first field flagged `is_string_repr`, if any.
    pub fn display_field(&self) -> Option<&Column> {
        self.fields.iter().find(|c| c.is_string_repr)
    }

    /// Fields safe to serialize with no unlocks: not `secret` and not `private`.
    pub fn public_fields(&self) -> impl Iterator<Item = &Column> {
        self.fields.iter().filter(|c| !c.secret && !c.private)
    }

    /// Serializable set, optionally adding back `private` fields for a caller
    /// that has authorized it: `!secret && (!private || allow_private)`.
    /// `secret` is never included either way.
    pub fn serializable_fields(&self, allow_private: bool) -> impl Iterator<Item = &Column> {
        self.fields
            .iter()
            .filter(move |c| !c.secret && (!c.private || allow_private))
    }

    /// True when the server populates this field, never the client: the primary
    /// key, or any `auto_*` column.
    pub fn is_server_managed(&self, f: &Column) -> bool {
        f.primary_key
            || f.auto_now
            || f.auto_now_add
            || f.auto_user
            || f.auto_user_add
            || f.auto_uuid
    }

    /// The safe client-writable set on create/update: not server-managed, not
    /// `noform`, not `privileged`. (`private`/`secret` are READ facts and do not
    /// affect writability.)
    pub fn writable_fields(&self) -> impl Iterator<Item = &Column> {
        self.fields
            .iter()
            .filter(|c| !self.is_server_managed(c) && !c.noform && !c.privileged)
    }

    /// Foreign-key columns (those carrying a `fk_target`).
    pub fn foreign_keys(&self) -> impl Iterator<Item = &Column> {
        self.fields.iter().filter(|c| c.fk_target.is_some())
    }
}
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p umbral-core --test model_exposure`
Expected: PASS — all 7 tests green.

- [ ] **Step 6: Lint, format, and full-crate build/test**

Run:
```bash
cargo fmt -p umbral-core
cargo clippy -p umbral-core --all-targets
cargo build -p umbral-core
cargo test -p umbral-core
```
Expected: no new clippy warnings attributable to `exposure.rs`; build + tests pass. (If clippy flags `needless_lifetimes` or similar on the `impl Iterator` returns, fix inline.)

- [ ] **Step 7: Commit**

```bash
git add crates/umbral-core/src/orm/exposure.rs \
        crates/umbral-core/src/orm/mod.rs \
        crates/umbral-core/tests/model_exposure.rs
git commit -m "feat(orm): model exposure contract — derived views on ModelMeta

Add field/display_field/public_fields/serializable_fields/is_server_managed/
writable_fields/foreign_keys as inherent methods on ModelMeta, isolated in
orm/exposure.rs (separate from the migration-snapshot code). Serializer
plugins read these facts instead of re-deriving field visibility/writability.

Spec: docs/decisions/2026-09-15-model-exposure-contract.md

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: User-facing doc page + facade reachability

**Files:**
- Create: `documentation/docs/v0.0.1/orm/model-exposure.mdx`
- Test: none (doc page); the "test" is the facade-reachability assertion folded into the doc's example being valid Rust.

**Interfaces:**
- Consumes: the seven `ModelMeta` methods from Task 1, reachable as `umbral::migrate::ModelMeta` (already re-exported from the facade at `crates/umbral/src/lib.rs:481`; inherent methods need no extra import).
- Produces: nothing code depends on.

- [ ] **Step 1: Confirm the facade path resolves**

Run:
```bash
grep -n "ModelMeta" crates/umbral/src/lib.rs
```
Expected: `pub use umbral_core::migrate::{… ModelMeta …}` (around line 481). This confirms `umbral::migrate::ModelMeta` is the path the doc example should use; no facade edit is needed because the methods are inherent.

- [ ] **Step 2: Confirm the `orm` docs area exists**

Run:
```bash
ls documentation/docs/v0.0.1/orm/_category_.json
```
Expected: the file exists (the `orm` area already has pages). If it does NOT exist, create it:
```json
{ "label": "ORM", "position": 3 }
```

- [ ] **Step 3: Write the doc page**

Create `documentation/docs/v0.0.1/orm/model-exposure.mdx`:

```mdx
---
title: Reading model metadata
description: The shared ModelExposure contract every serializer plugin reads to learn a model's shape.
sidebar_position: 90
---

# Reading model metadata

Every plugin that turns a model into an external representation — REST, GraphQL,
the admin, a plugin you write — needs the same facts about a model: which fields
are safe to serialize, which a client may write, which column is the display
string, which are foreign keys. Rather than each plugin re-deriving these,
`ModelMeta` exposes them as derived views. You read them from the model registry
(`umbral::registered_models()`) or from a concrete type via
`ModelMeta::for_::<T>()`.

The rule of thumb: **facts live on the model, policy lives in your plugin.**
`ModelMeta` tells you a field is `secret` or `private`; your plugin decides who
may unlock it.

## One example

```rust
use umbral::migrate::ModelMeta;

fn serialize_response(meta: &ModelMeta, staff: bool) -> Vec<String> {
    // Facts from the model: everything safe to serialize, adding back private
    // fields only for a staff caller. `secret` is never included.
    meta.serializable_fields(staff)
        .map(|col| col.name.clone())
        .collect()
}

fn writable(meta: &ModelMeta) -> Vec<String> {
    // The safe client-writable set: no primary key, no auto_* column, no
    // privileged (mass-assignment-guarded) field.
    meta.writable_fields().map(|c| c.name.clone()).collect()
}
```

Other derived views: `field(name)`, `display_field()`, `public_fields()`,
`is_server_managed(col)`, and `foreign_keys()`. Declared presentation intent
(`list_display`, `search_fields`, `ordering`, `readonly_fields`, …) and raw
per-field facts (`help`, `widget`, `choices`) are read directly off the public
`ModelMeta` / `Column` fields.

## Design rationale

See the design note: `docs/decisions/2026-09-15-model-exposure-contract.md`.
```

- [ ] **Step 4: Commit**

```bash
git add documentation/docs/v0.0.1/orm/model-exposure.mdx
git commit -m "docs(orm): user page for the model exposure contract

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Follow-up (out of scope for this plan)

Per the spec's migration path, plugin adoption is separate, independently-shippable work — each its own plan/commit after this contract lands:

- `umbral-rest`: replace hand-rolled serializable/writable derivations with `meta.serializable_fields(..)` / `meta.writable_fields()`; keep `ResourceConfig.hidden`, permissions, throttles as policy overlay.
- `umbral-admin`: back field-visibility and default `list_display`/`readonly` on the contract.
- `umbral-graphql`: read the contract for type/field generation.
- Slice #2 (separate spec): **computed fields** as a model-level fact (needs a non-serialized evaluator registry, since values are code not snapshot data).

## Self-Review

- **Spec coverage:** The spec's v1 surface (7 methods) → Task 1. The isolation requirement (exposure.rs, not migrate.rs) → Task 1 Files + module doc header. Facade exposure ("no change needed") → Task 2 Step 1 verifies. "Ship a feature, ship its doc page" (CLAUDE.md) → Task 2. Testing strategy (behavioural, static path; note registry serves identical value) → Task 1 tests + the test-file doc comment. Deferred items (computed fields, REST/admin/graphql adoption, trait extraction) → Follow-up section. No spec requirement is left without a task.
- **Placeholder scan:** none — all code blocks are complete; the only deferral is the explicitly out-of-scope Follow-up section.
- **Type consistency:** method names and signatures in Task 1's Produces block, the module code, and the test all match (`field`, `display_field`, `public_fields`, `serializable_fields`, `is_server_managed`, `writable_fields`, `foreign_keys`); `Column` field names (`primary_key`, `secret`, `private`, `noform`, `privileged`, `is_string_repr`, `auto_*`, `fk_target`, `name`) match `migrate.rs`.
