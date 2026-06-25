# `M2M<T>` — many-to-many relations with auto-generated junction tables

Date: 2026-06-02
Status: Design only. Filed in gaps.md #61.2 as deferred. Targeting a focused future session.

## The user-visible shape

A model declares an `M2M<T>` field. The owning struct gains no SQL column on its own table; the framework auto-generates a junction table at migration time, exposes a collection accessor on the parent model, and the admin renders a multi-select FK picker against the related table.

```rust
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "permissions_group")]
pub struct Group {
    pub id: i64,
    #[umbral(string, max_length = 150)]
    pub name: String,
    pub description: Option<String>,
    /// Auto-generated junction table `permissions_group_permissions`
    /// with columns `(group_id BIGINT REFERENCES permissions_group(id),
    /// permission_id TEXT REFERENCES permissions_permission(codename))`.
    /// Composite PK (group_id, permission_id) gives free uniqueness.
    pub permissions: M2M<Permission>,
}
```

Today's hand-rolled equivalent: a `permissions_grouppermission` model whose rows the admin lets staff users edit one-by-one. The `M2M<T>` shape collapses that to one field declaration and the framework handles the rest.

## Why this is gap #61.2 not gap #61.1

Gap #61 part 1 (revert noedits) was a one-commit policy change. Part 2 is a new field type that touches:

- ORM (new `M2M<T>` struct, accessors, serialisation)
- Macros (recognise `M2M<T>` like `ForeignKey<T>` is recognised today; emit accessors)
- Migration engine (junction table auto-emit, `M2M` add/drop ops)
- Admin (collection-valued FK picker — different widget from the single-FK `ForeignKey<T>` combobox)
- REST (serialise as an array of related PKs / full objects with `select_related`)
- Tests (integration coverage for each layer)

Comparable in scope to gap #60's full `ForeignKey<T>` PK generalisation. Single-session execution risks finishing in a partially-broken state. The design note here captures the shape so a focused future session can move fast.

## Mechanics

### Field type

```rust
// In crates/umbral-core/src/orm/m2m.rs
pub struct M2M<T: Model> {
    /// Resolved related rows when the parent was loaded with
    /// `.prefetch_related("field_name")`. `None` = not loaded.
    resolved: Option<Vec<T>>,
    /// Cached parent-row PK so accessor methods know which `WHERE`
    /// clause to apply. Set by the FromRow path on the owning model.
    parent_id: Option<i64>,  // genericised once FK<T> change is wider
    _phantom: PhantomData<T>,
}

impl<T: Model> M2M<T> {
    /// Add a relation. Inserts one row into the junction table.
    pub async fn add(&mut self, related: &T) -> Result<(), WriteError> { ... }

    /// Add many at once. One `INSERT ... VALUES (..), (..), ...`.
    pub async fn add_many(&mut self, related: &[T]) -> Result<(), WriteError> { ... }

    /// Remove a relation. `DELETE FROM <junction> WHERE parent = ? AND child = ?`.
    pub async fn remove(&mut self, related: &T) -> Result<(), WriteError> { ... }

    /// Replace the entire relation set. Delete-all + bulk insert in one tx.
    pub async fn set(&mut self, related: &[T]) -> Result<(), WriteError> { ... }

    /// Drop every relation. `DELETE FROM <junction> WHERE parent = ?`.
    pub async fn clear(&mut self) -> Result<(), WriteError> { ... }

    /// Lazy fetch — runs a JOIN against the junction. Mirrors
    /// `prefetch_related` but on-demand.
    pub async fn fetch(&self) -> Result<Vec<T>, sqlx::Error> { ... }

    /// Read the cached set when `prefetch_related` populated it; None
    /// otherwise.
    pub fn resolved(&self) -> Option<&[T]> { self.resolved.as_deref() }
}
```

The struct has **no column** on the parent table. `Model::FIELDS` excludes M2M fields; the migration engine treats them as a separate output stream (junction-table CreateOps).

### Junction table

Naming convention: `<parent_table>_<m2m_field_name>`. The field name is plural in the canonical case (`permissions` → `permissions_group_permissions`); the framework doesn't try to depluralise. Columns:

```sql
CREATE TABLE permissions_group_permissions (
    group_id      BIGINT NOT NULL REFERENCES permissions_group(id) ON DELETE CASCADE,
    permission_id TEXT   NOT NULL REFERENCES permissions_permission(codename) ON DELETE CASCADE,
    PRIMARY KEY (group_id, permission_id)
);
```

- Composite PK enforces uniqueness — adding the same (group, permission) twice is a silent no-op via `ON CONFLICT DO NOTHING`.
- `ON DELETE CASCADE` on both sides matches Django's default. A removed parent or related row cleans up its junction rows.
- Both FK column types come from the FK-target's `Model::PrimaryKey` lookup — same `fk_target_pk` helper introduced in gap #60.

### Macro extension

The `classify_field_type` function already detects `ForeignKey<T>` and `NullableForeignKey<T>`. Add a `FieldKind::Many2Many(Box<Type>)` branch. The derive emits:

- An `M2M<T>` field initialiser in `FromRow` that captures the parent PK.
- An `m2m_specs() -> &[M2MSpec]` static for each model carrying `(field_name, target_table, target_pk_col)`. The migration engine reads this in addition to `FIELDS`.

### Migration engine

Add an `Operation::CreateM2M { junction_table, parent_table, parent_col, child_table, child_col }` variant. Diff time:

- For each model, walk `m2m_specs()`. New entries → `CreateM2M`. Removed entries → `DropTable` against the junction.
- The autodetector renames a field by name-stable diff (same target table, same field name → same junction; field name change → drop + create the junction, with a stderr warning like the second-pass FK rename detector).

### Admin

Two pieces:

1. **Form rendering.** The admin currently distinguishes column types via `Column::ty`. M2M fields aren't columns, so the meta layer needs a parallel `model.m2m_relations` list. The form widget is a checkbox group (small N) or an async combobox with chips (large N) — same JS as the existing FK picker, but with `multiple` enabled. The submitted value is an array of related PKs.

2. **Detail view.** Renders the resolved set as a comma-separated list of `__str__` representations, with each item linking to the related row's detail page. `prefetch_related` runs against the registry to hydrate the value before render.

### REST

`Resource::list` and `retrieve` emit M2M fields as an array of related PKs by default. With `select_related="permissions"` (REST parlance — same surface as the admin) the array becomes an array of full related objects. `Create` / `update` accept the same shape on the way in; the framework reconciles the junction in one transaction.

## Open questions for the build session

1. **Reverse accessor.** Django's `Permission.group_set.all()` is the symmetric M2M traversal. The framework can synthesize a `reverse_m2m::<Group>()` helper on `Permission` at derive time iff Permission's derive runs after Group's — which it might not, depending on declaration order. Resolution at boot rather than derive (walk the registered models, build the reverse map) is the right shape.

2. **Through models.** Django's `through="MyJunction"` lets the user add extra columns to the junction (a `granted_at: DateTime` on `group ↔ permission`). v1 of `M2M<T>` is auto-junction-only; the user wanting extras keeps using an explicit junction model (the `GroupPermission` shape that exists today). Through-model support lands in v2.

3. **Cascade semantics.** `ON DELETE CASCADE` for the junction is non-negotiable for the auto shape. But cascading FROM the junction (deleting a junction row triggering parent deletion) doesn't apply — the junction is owned by the framework, not user code.

4. **Cross-DB M2M.** Multi-DB routing (gap #53) introduced per-model database aliases. If Group lives on DB-A and Permission lives on DB-B, the junction lives on... one of them, and the FK to the other is cross-DB (which neither SQLite nor Postgres supports natively). Reject cross-DB M2M at boot with a clear error; the user defines an explicit join model instead.

## What's deferred and why

- Through-models (v2).
- Reverse accessor codegen (needs registry-walk at boot, not derive-time).
- Symmetric M2M (a `Friend = M2M<User>` on `User` itself, with no parent/child distinction).
- Auto-removal of stale junction rows on related-row update (the FK already handles this via CASCADE).

The minimum-useful v1 is enough to retire `GroupPermission` / `UserGroup` / `UserPermission` as user-facing models — they'd become framework-generated junctions, and `Group { permissions: M2M<Permission> }` / `AuthUser { groups: M2M<Group> }` / `AuthUser { permissions: M2M<Permission> }` would be the user-facing shape.
