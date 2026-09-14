# ORM Heavy Relations — Plan A: Unify the hop→JOIN engine

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Converge the three diverging hop→JOIN SQL builders onto one `RelPath`/`walk_joins` engine and one string resolver, so every consumer (traversal terminals, `select_related`/`join_related`/`values`, and the later aggregate/hydration features) turns a relation path into JOIN SQL through a single code path.

**Architecture:** Keep `RelPath`/`HopSpec` (the richer, direction- and M2M-aware representation) as canonical. Add `PathBase::TableRoot` for non-PK-anchored uses and a `RelPath::from_path::<T>(&str)` resolver that replaces `resolve_join_hops` + `resolve_m2m_chain`. Generalise the existing `walk_forward_joins` into `walk_joins` (all hop kinds + null policy + alias prefix). Rebuild every caller on it, then delete the old builders. Refactor strictly under the existing green test suite; the only intended behavior change is `select_related`'s silent-skip → loud error.

**Tech Stack:** Rust, `sea-query` / `sea-query-binder` (SQL building), `sqlx` (SQLite + Postgres), `#[derive(Model)]` proc-macro. No raw SQL.

**Spec:** `docs/specs/orm-heavy-relations-epic.md` (sub-project A).

## Global Constraints

- ORM-only: every JOIN/subquery emitted through sea-query; no `sqlx::query("...")` string SQL. (CLAUDE.md "Plugins use the ORM. Not raw SQL.")
- Multi-tenant: every table reference goes through `crate::db::router::schema_qualified_table(...)`.
- Backends: SQLite is the required test backend; Postgres parity by symmetry, any PG-only assertion gated behind `UMBRAL_TEST_POSTGRES_URL`.
- Ambient pool preserved: no new `.on(&pool)` threading in accessor paths.
- Before each commit: `cargo fmt && cargo clippy --all-targets && cargo build && cargo test` (whole workspace) must pass.
- Behavioral tests: real rows, the public path, read the graph back — never a SQL-string assertion as the sole assertion (a `to_sql`/query-count check may run *alongside* a round-trip).
- Never wipe a DB or delete migration files to make a test pass.
- **Security & Performance (binds every task — see the spec's "Security & Performance" section):**
  - `walk_joins` schema-qualifies EVERY joined table (root, intermediates, junctions, leaf) via `schema_qualified_table` — a miss crosses tenant boundaries.
  - The unification must NOT make a single hop heavier: the lightweight single-hop form in `to_one_hop` / `single_to_many_queryset` is preserved; the multi-table JOIN path is for genuine multi-hop only. A single-hop accessor's query shape/count is unchanged before vs. after A (regression-tested in Task 3).
  - Row scoping is not bypassed by a JOIN: soft-delete (`deleted_at IS NULL`) on a traversed table is applied consistently with a direct read, OR its non-application is a documented, tested decision (Task 3).
  - `Masked<T>`/encrypted columns crossing a hop decrypt through the same path as a direct read — no raw-column read that bypasses decryption.
  - Review lens: flag any query-count regression, dropped `schema_qualified_table`/row-scoping on a joined table, or single hop routed through the heavy JOIN path.

## File Structure

- `crates/umbral-core/src/orm/relation.rs` — add `PathBase::TableRoot`; add `RelPath::from_path`; the `NullJoinPolicy` enum. Owns the path model + string resolver.
- `crates/umbral-core/src/orm/queryset/relation_resolve.rs` — `walk_forward_joins` → `walk_joins` (all hop kinds, null policy, alias prefix); rebuild `build_to_one_select` / `build_leaf_select` / `build_prefix_pivot_subquery` on it; drop the deferred `TODO`.
- `crates/umbral-core/src/orm/queryset/mod.rs` — rebuild `apply_join_related` on `walk_joins`; delete `JoinHop` / `resolve_join_hops` / `resolve_join_hops_for` / `resolve_m2m_chain`; make `select_related` resolution loud.
- `crates/umbral-core/tests/relation_from_path.rs` (new) — `RelPath::from_path` resolver unit-ish tests.
- `crates/umbral-core/tests/select_related_loud_error.rs` (new) — the behavior-change test.
- `documentation/docs/v0.0.1/orm/select-related.mdx` (new or edit) — note the loud-error behavior.

---

### Task 1: `PathBase::TableRoot` + `NullJoinPolicy`

**Files:**
- Modify: `crates/umbral-core/src/orm/relation.rs` (the `PathBase` enum ~line 114; add a new enum near it)

**Interfaces:**
- Produces: `PathBase::TableRoot { table: &'static str }`; `pub enum NullJoinPolicy { Inner, LeftForNullable }`.

- [ ] **Step 1: Write the failing test** (append to `crates/umbral-core/tests/relation_handle.rs`)

```rust
#[test]
fn table_root_base_constructs() {
    use umbral::orm::relation::{PathBase, NullJoinPolicy};
    let b = PathBase::TableRoot { table: "auth_user" };
    match b {
        PathBase::TableRoot { table } => assert_eq!(table, "auth_user"),
        _ => panic!("wrong variant"),
    }
    // exhaustiveness / Copy sanity for the policy enum
    let p = NullJoinPolicy::LeftForNullable;
    assert_ne!(p, NullJoinPolicy::Inner);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbral-core --test relation_handle table_root_base_constructs`
Expected: FAIL (`no variant TableRoot`, `NullJoinPolicy` undefined).

- [ ] **Step 3: Write minimal implementation** (in `relation.rs`)

Add to `PathBase`:
```rust
    /// A path rooted at a table itself (no specific row) — the base for
    /// `select_related` / aggregate JOINs, which hang off the outer query's
    /// own FROM rather than a `WHERE pk = ?`.
    TableRoot { table: &'static str },
```
Add near the path model:
```rust
/// Whether a nullable hop LEFT-joins (keep the parent row) or INNER-joins
/// (a null link drops the row). Traversal uses `Inner`; hydration /
/// `select_related` uses `LeftForNullable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NullJoinPolicy {
    Inner,
    LeftForNullable,
}
```
Ensure both are re-exported: check `crate::orm` re-exports `relation::{PathBase, NullJoinPolicy}` and the facade `umbral::orm::relation` path resolves (it already exposes `PathBase`).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p umbral-core --test relation_handle table_root_base_constructs`
Expected: PASS. Then `cargo build -p umbral-core` — fix any non-exhaustive `match path.base` sites the new variant introduces (there are two, in `relation_resolve.rs`: `build_to_one_select`, `build_leaf_select`; for now add an arm `PathBase::TableRoot { .. } => return Err(protocol_error("TableRoot base has no pk to anchor a single-object traversal"))` — Task 3 revisits).

- [ ] **Step 5: Commit**

```bash
git add crates/umbral-core/src/orm/relation.rs crates/umbral-core/tests/relation_handle.rs
git commit -m "feat(orm): PathBase::TableRoot + NullJoinPolicy for the unified walker"
```

---

### Task 2: `RelPath::from_path::<T>(&str)` string resolver

**Files:**
- Modify: `crates/umbral-core/src/orm/relation.rs`
- Test: `crates/umbral-core/tests/relation_from_path.rs` (new)

**Interfaces:**
- Consumes: `PathBase::TableRoot` (Task 1), `HopSpec`, `HopKind`, `JunctionSpec`, `crate::migrate::registered_models()`, `T::FIELDS` (`FieldSpec { name, fk_target: Option<&str>, nullable, unique, .. }`), `T::M2M_RELATIONS`, `T::REVERSE_FK_RELATIONS` (`&[ReverseFkRelationSpec]`, `model.rs:487`).
- Produces: `pub fn RelPath::from_path<T: Model>(path: &str) -> Result<RelPath, sqlx::Error>` — a `RelPath` with `base = TableRoot { table: T::TABLE }` and one `HopSpec` per `__` segment, resolving **forward FK/O2O, M2M, and reverse-FK** segments at any depth. Errors loudly on an unresolved or ambiguous segment.

- [ ] **Step 1: Write the failing test** (`crates/umbral-core/tests/relation_from_path.rs`)

Reuse the models from `relation_codegen.rs` (a `Post { author: ForeignKey<User> }`, a `User { company: ForeignKey<Company> }`, and a model with an M2M). Register them via the same App/boot helper those tests use (copy the `#[tokio::test]` App-setup preamble from `relation_codegen.rs`).

```rust
#[tokio::test]
async fn from_path_resolves_two_hop_fk_chain() {
    // App/registry setup (copied from relation_codegen.rs preamble) …
    use umbral::orm::relation::{RelPath, PathBase, HopKind};
    let p = RelPath::from_path::<Post>("author__company").expect("resolves");
    match p.base { PathBase::TableRoot { table } => assert_eq!(table, Post::TABLE), _ => panic!() }
    assert_eq!(p.hops.len(), 2);
    assert_eq!(p.hops[0].kind, HopKind::Fk);
    assert_eq!(p.hops[0].to_table, User::TABLE);
    assert_eq!(p.hops[1].to_table, Company::TABLE);
    assert!(p.hops.iter().all(|h| h.fk_on_from)); // forward FKs
}

#[tokio::test]
async fn from_path_resolves_m2m_first_segment() {
    // … setup with a model M that has an M2M field `tags` -> Tag
    use umbral::orm::relation::{RelPath, HopKind};
    let p = RelPath::from_path::<Developer>("software_groups").expect("resolves");
    assert_eq!(p.hops.len(), 1);
    assert_eq!(p.hops[0].kind, HopKind::M2M);
    assert!(p.hops[0].junction.is_some());
}

#[tokio::test]
async fn from_path_resolves_reverse_fk_two_hop() {
    // … setup: User <- Post(author FK) <- Comment(post FK) …
    use umbral::orm::relation::{RelPath, HopKind};
    let p = RelPath::from_path::<User>("posts__comments").expect("resolves");
    assert_eq!(p.hops.len(), 2);
    assert_eq!(p.hops[0].kind, HopKind::ReverseFk); // User <- Post
    assert!(!p.hops[0].fk_on_from);                 // FK lives on the child
    assert_eq!(p.hops[1].kind, HopKind::ReverseFk); // Post <- Comment
    assert_eq!(p.hops[1].to_table, Comment::TABLE);
}

#[tokio::test]
async fn from_path_unknown_segment_errors_loudly() {
    // … setup …
    use umbral::orm::relation::RelPath;
    let err = RelPath::from_path::<Post>("athor").unwrap_err(); // typo
    assert!(err.to_string().contains("athor"), "error names the bad segment: {err}");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbral-core --test relation_from_path`
Expected: FAIL (`from_path` undefined).

- [ ] **Step 3: Write minimal implementation** (in `relation.rs`, `impl RelPath`)

```rust
pub fn from_path<T: Model>(path: &str) -> Result<RelPath, sqlx::Error> {
    let registered = crate::migrate::registered_models();
    let segs: Vec<&str> = path.split("__").filter(|s| !s.is_empty()).collect();
    if segs.is_empty() {
        return Err(protocol_error("empty relation path"));
    }
    let mut hops: Vec<HopSpec> = Vec::with_capacity(segs.len());
    let mut current_table: &'static str = T::TABLE;

    // Hop 0 off the typed parent: FK/O2O field, or M2M field.
    if let Some(f) = T::FIELDS.iter().find(|f| f.name == segs[0]) {
        let tgt = f.fk_target.ok_or_else(|| protocol_error(&format!(
            "`{}` on `{}` is not a relation (no fk_target)", segs[0], T::NAME)))?;
        hops.push(HopSpec {
            kind: HopKind::Fk, // O2OForward if the FK is unique — see note
            from_table: T::TABLE, to_table: tgt, fk_column: f.name,
            fk_on_from: true, required: !f.nullable, junction: None,
        });
        current_table = tgt;
    } else if let Some(rel) = T::M2M_RELATIONS.iter().find(|r| r.field_name == segs[0]) {
        hops.push(HopSpec {
            kind: HopKind::M2M,
            from_table: T::TABLE, to_table: rel.target_table, fk_column: "",
            fk_on_from: true,
            required: false,
            junction: Some(JunctionSpec {
                table: rel.junction_table, // confirm exact field names on the M2M rel struct
                parent_column: rel.parent_column, target_column: rel.target_column,
            }),
        });
        current_table = rel.target_table;
    } else if let Some(rev) = reverse_fk_lookup::<T>(T::TABLE, segs[0]) {
        // Reverse FK: many child rows point back at this row. The FK column
        // lives on the CHILD (to_table); `fk_on_from = false`.
        hops.push(HopSpec {
            kind: HopKind::ReverseFk,
            from_table: T::TABLE, to_table: rev.child_table, fk_column: rev.fk_column,
            fk_on_from: false, required: false, junction: None,
        });
        current_table = rev.child_table;
    } else {
        return Err(protocol_error(&format!(
            "unknown relation `{}` on `{}`", segs[0], T::NAME)));
    }

    // Deeper hops read the migrate registry for `current_table` — forward FK
    // (a column with an fk_target) OR reverse FK (a child table whose column
    // points back at `current_table`).
    for seg in &segs[1..] {
        let meta = registered.iter().find(|m| m.table == current_table)
            .ok_or_else(|| protocol_error(&format!("table `{current_table}` not registered")))?;
        if let Some(col) = meta.fields.iter().find(|c| c.name == *seg) {
            // forward FK segment
            let tgt = col.fk_target.as_deref().ok_or_else(|| protocol_error(&format!(
                "`{seg}` on `{current_table}` is not a foreign key")))?;
            hops.push(HopSpec {
                kind: HopKind::Fk, from_table: intern(current_table), to_table: intern(tgt),
                fk_column: intern(seg), fk_on_from: true,
                required: !col.nullable, junction: None,
            });
            current_table = intern(tgt);
        } else if let Some(rev) = reverse_fk_lookup_registry(&registered, current_table, seg) {
            // reverse FK segment (child table points back at current_table)
            hops.push(HopSpec {
                kind: HopKind::ReverseFk, from_table: intern(current_table),
                to_table: rev.child_table, fk_column: rev.fk_column,
                fk_on_from: false, required: false, junction: None,
            });
            current_table = rev.child_table;
        } else {
            return Err(protocol_error(&format!(
                "unknown relation `{seg}` on `{current_table}`")));
        }
    }
    Ok(RelPath { base: PathBase::TableRoot { table: T::TABLE }, hops })
}
```

**Reverse-FK resolution (fold in — B's `posts__comments` and C both need it).** A path segment can name a reverse FK (children pointing back), not just a forward FK/M2M. Two helpers:
- `reverse_fk_lookup::<T>(table, seg)` — hop-0 off the typed root: consult `T::REVERSE_FK_RELATIONS` (`&[ReverseFkRelationSpec]`, `crates/umbral-core/src/orm/model.rs:420`, struct at `:487`), matching `seg` against the declared relation name / `<child_table>_set` form (mirror the name-matching the existing reverse accessor uses — see `queryset/mod.rs:409`, `:439` `discoverable.push(format!("{}_set", meta.table))`). Returns `{ child_table, fk_column }`.
- `reverse_fk_lookup_registry(registered, current_table, seg)` — deeper hops (no per-model const available for an intermediate table): scan `registered` for any model with a column whose `fk_target == current_table`, matching `seg` to `<child_table>` / `<child_table>_set`. **Ambiguity:** if two child models point back, the bare name is ambiguous — return a loud `Err` naming the candidates and requiring the disambiguated form (same posture as the typed `<child>_via_<field>_set()` accessor). Returns `{ child_table: &'static str (interned), fk_column: &'static str (interned) }`.

Note (resolve during implementation, do not defer): `HopSpec` fields are `&'static str`, but registry names are owned `String`. Pick ONE and apply consistently — **(a)** change `HopSpec`'s `from_table`/`to_table`/`fk_column` to `Cow<'static, str>` (update the derive's literals to `Cow::Borrowed`); or **(b)** an `intern(&str) -> &'static str` helper backed by a process `OnceLock<Mutex<HashSet<&'static str>>>`. The `intern(...)` calls above assume (b); if you choose (a), drop them and assign the `Cow` directly. Prefer (a) unless the derive churn is large. Also set `O2OForward` vs `Fk` for a forward hop from the field's uniqueness (`FieldSpec` unique flag).

Note (resolve during implementation, do not defer): `HopSpec` fields are `&'static str`, but registry `Column` names are owned `String`. Two clean options — pick one and apply consistently: **(a)** change `HopSpec`'s `to_table`/`fk_column`/`from_table` to `Cow<'static, str>` (touches the derive's `HopSpec` literals — set them to `Cow::Borrowed`); or **(b)** intern registry-derived names via a process `OnceLock<Mutex<HashSet<&'static str>>>` string interner returning `&'static str`. Option (a) is cleaner and matches "fix the contract"; prefer it unless the derive churn is large. Also determine `O2OForward` vs `Fk` from the field's uniqueness (`FieldSpec` unique flag) so the hop kind is exact.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p umbral-core --test relation_from_path`
Expected: PASS all three.

- [ ] **Step 5: Commit**

```bash
git add crates/umbral-core/src/orm/relation.rs crates/umbral-core/tests/relation_from_path.rs
git commit -m "feat(orm): RelPath::from_path string resolver (replaces resolve_join_hops/resolve_m2m_chain)"
```

---

### Task 3: `walk_joins` — the one walker for every hop kind

**Files:**
- Modify: `crates/umbral-core/src/orm/queryset/relation_resolve.rs`

**Interfaces:**
- Consumes: `HopSpec`, `HopKind`, `JunctionSpec`, `NullJoinPolicy` (Task 1), `schema_qualified_table`, `registered_models`.
- Produces: `pub(crate) fn walk_joins(select: &mut SelectStatement, root_alias: Alias, hops: &[HopSpec], policy: NullJoinPolicy, prefix: &str, registered: &[ModelMeta]) -> Result<Alias, sqlx::Error>` — appends one JOIN per hop (FK/O2O forward `near.fk = far.pk`; reverse-O2O `near.pk = far.fk`; M2M via junction; reverse-FK `near.pk = far.fk`), returns the leaf alias. `LeftForNullable` uses `JoinType::LeftJoin` when `!hop.required`, else INNER.

- [ ] **Step 1: Write the failing test** (`relation_resolve.rs` has no test module today; add an integration test in a new `crates/umbral-core/tests/walk_joins_sql.rs`, driving through `RelPath::to_sql` on a hand-built path — the public probe surface)

```rust
#[tokio::test]
async fn walk_joins_emits_inner_join_for_forward_fk_chain() {
    // App setup registering Post/User/Company (as in relation_codegen.rs) …
    use umbral::orm::relation::RelPath;
    let path = RelPath::from_path::<Post>("author__company").unwrap();
    // wrap into a Relation<Company> to reach to_sql, OR expose a test helper:
    let sql = umbral::orm::relation::to_sql_for_path::<Company>(&path).unwrap();
    assert!(sql.contains("INNER JOIN"), "sql: {sql}");
    assert!(sql.matches("JOIN").count() >= 2, "two hops -> two joins: {sql}");
}

// SECURITY: every joined table is schema-qualified (multi-tenant isolation).
// If a schema router is hard to install in a unit test, at minimum assert the
// leaf/intermediate table names appear as sea-query-quoted identifiers routed
// through schema_qualified_table (grep the builder, or run under a test router
// that prefixes a schema and assert the prefix appears on EVERY table).
#[tokio::test]
async fn walk_joins_schema_qualifies_every_hop() {
    // install the repo's test schema router (see db::router tests:
    // schema_router_qualifies_table_references) mapping tables -> "tenant1".
    // Build a 2-hop path and assert BOTH hop tables appear schema-qualified.
    use umbral::orm::relation::RelPath;
    let path = RelPath::from_path::<Post>("author__company").unwrap();
    let sql = umbral::orm::relation::to_sql_for_path::<Company>(&path).unwrap();
    assert_eq!(sql.matches("tenant1").count(), 3, "root+2 hops all qualified: {sql}");
}

// PERFORMANCE: a single hop keeps its lightweight shape — the unification must
// not route one hop through the heavy multi-table JOIN plan. Assert the
// single-hop accessor's SQL is the pre-A lightweight form (a direct filter /
// junction subquery), NOT a multi-JOIN. Capture the pre-A SQL string in the
// test as the golden and assert equality (behavior-preserving refactor).
#[tokio::test]
async fn single_hop_stays_lightweight() {
    // post.author() is ONE forward FK. Its resolved SQL must be the
    // single-hop form (one WHERE on the FK value / one subquery), with no
    // second table joined. Assert the JOIN count is 0 or 1 (the single-hop
    // form), never the deep multi-JOIN plan.
    use umbral::orm::relation::RelPath;
    let path = RelPath::from_path::<Post>("author").unwrap();
    let sql = umbral::orm::relation::to_sql_for_path::<User>(&path).unwrap();
    assert!(sql.matches(" JOIN ").count() <= 1, "single hop must stay lightweight: {sql}");
}
```

Also add, in the same file, a soft-delete scoping test (SECURITY): register a model with `#[umbral(soft_delete)]` as a traversal target, soft-delete a leaf row, and assert a multi-hop traversal does NOT return it — OR, if the framework's decision is that a JOIN does not re-apply the related manager's soft-delete scope (Django-style), assert that documented behavior explicitly and record the decision in the ledger. Resolve which behavior is correct during implementation by checking whether `resolve_leaf_queryset` applies `soft_delete_active` to intermediate tables today; make the traversal consistent with a direct read and document any deliberate exception.

(If `to_sql_for_path` doesn't exist, add a thin `pub fn to_sql_for_path<Leaf: Model>(path: &RelPath) -> Result<String, sqlx::Error>` in `relation.rs` delegating to `relation_resolve::to_one_sql::<Leaf>(path, false)` — it is a probe surface, keep it.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbral-core --test walk_joins_sql`
Expected: FAIL (helper/behavior missing) OR the SQL lacks the second join because the old `walk_forward_joins` is still forward-only. (It already handles forward FK, so this specific test may pass early; the real new coverage is the M2M/reverse arms below — add those assertions once `walk_joins` exists.)

- [ ] **Step 3: Write minimal implementation**

Rename `walk_forward_joins` → `walk_joins`, add the `policy` and `prefix` params, and extend the per-hop `match hop.kind`:
```rust
fn walk_joins(select: &mut SelectStatement, root_alias: Alias, hops: &[HopSpec],
    policy: NullJoinPolicy, prefix: &str, registered: &[ModelMeta]) -> Result<Alias, sqlx::Error> {
    let mut near = root_alias;
    for (idx, hop) in hops.iter().enumerate() {
        let far = Alias::new(format!("{prefix}{}", idx + 1));
        let jt = match policy {
            NullJoinPolicy::LeftForNullable if !hop.required => JoinType::LeftJoin,
            _ => JoinType::InnerJoin,
        };
        match hop.kind {
            HopKind::Fk | HopKind::O2OForward => { /* near.fk = far.pk (existing forward arm) */ }
            HopKind::O2OReverse => { /* near.pk = far.fk (existing reverse arm) */ }
            HopKind::ReverseFk => { /* near.pk = far.fk, fk on far table */ }
            HopKind::M2M => {
                let j = hop.junction.ok_or_else(|| protocol_error("M2M hop missing junction"))?;
                let jalias = Alias::new(format!("{prefix}j{idx}"));
                // near.pk = junction.parent_column ; junction.target_column = far.pk
                select.join_as(jt, schema_qualified_table(j.table), jalias.clone(),
                    Expr::col((near.clone(), Alias::new(pk_of(registered, hop.from_table)?)))
                        .equals((jalias.clone(), Alias::new(j.parent_column))));
                select.join_as(jt, schema_qualified_table(hop.to_table), far.clone(),
                    Expr::col((jalias, Alias::new(j.target_column)))
                        .equals((far.clone(), Alias::new(pk_of(registered, hop.to_table)?))));
                near = far; continue;
            }
        }
        near = far;
    }
    Ok(near)
}
```
Keep the existing forward/reverse-O2O ON-clause bodies (they already exist in `walk_forward_joins`). `pk_of` already exists in this module. Update the two existing callers (`build_to_one_select`, `build_prefix_pivot_subquery`) to pass `NullJoinPolicy::Inner` and `prefix = "__rel_"`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p umbral-core --test walk_joins_sql` then the full traversal suite:
`for t in relation_traversal_integration relation_deep_to_one relation_deep_to_many relation_m2m_query values_traversal filter_relation_traversal; do cargo test -p umbral-core --test $t; done`
Expected: all PASS (no regression; the walker is behavior-identical for existing paths).

- [ ] **Step 5: Commit**

```bash
git add crates/umbral-core/src/orm/queryset/relation_resolve.rs crates/umbral-core/src/orm/relation.rs crates/umbral-core/tests/walk_joins_sql.rs
git commit -m "refactor(orm): generalise walk_forward_joins into walk_joins (all hop kinds, null policy)"
```

---

### Task 4: Rebuild `apply_join_related` on `walk_joins`; delete the old builders

**Files:**
- Modify: `crates/umbral-core/src/orm/queryset/mod.rs` (`apply_join_related` ~1650; delete `JoinHop` ~1320, `resolve_join_hops` ~1341, `resolve_join_hops_for` ~1379, `resolve_m2m_chain` ~1388)

**Interfaces:**
- Consumes: `RelPath::from_path` (Task 2), `walk_joins` (Task 3), `NullJoinPolicy`.
- Produces: `apply_join_related` builds its JOINs via `walk_joins(policy = Inner, prefix = "__j_")` off `RelPath::from_path::<T>(field_name)`. `select_related` still projects the joined FK columns (behavior unchanged); `values`-traversal still routes here.

- [ ] **Step 1: Confirm the regression net compiles the intent** — no new test yet; this task is a refactor guarded by the existing `select_related` / `join_related` / `values` / `prefetch` suites. List them:
`select_related select_related_nested join_related join_related_m2m joins_nested values_traversal values_projection prefetch_related reverse_fk_prefetch first_hydrates_relations query_counts`

- [ ] **Step 2: Run them GREEN before touching code**

Run: `for t in select_related select_related_nested join_related join_related_m2m joins_nested values_traversal prefetch_related query_counts; do echo "== $t"; cargo test -p umbral-core --test $t 2>&1 | grep 'test result'; done`
Expected: all PASS (baseline).

- [ ] **Step 3: Rewrite `apply_join_related`** to resolve each `join`/`select_related` field via `RelPath::from_path::<T>(field_name)` and emit JOINs with `walk_joins`, then project the leaf/hop columns exactly as before (keep the projection + alias-to-bare-name logic; only the JOIN emission changes). Delete `JoinHop`, `resolve_join_hops`, `resolve_join_hops_for`, `resolve_m2m_chain` and fix every reference the compiler flags (the M2M-first branch now comes from `from_path` producing an `M2M` first hop; `walk_joins` handles it). Remove the `TODO(orm-traversal, deferred)` breadcrumb block at the top of `relation_resolve.rs`.

- [ ] **Step 4: Run the full regression net + workspace**

Run: the Step-2 loop again, then `cargo test` (whole workspace).
Expected: all PASS, identical results to the baseline. If any `values`/`select_related` test changes output, STOP — that is an unintended behavior change; diff and fix the projection, do not edit the test.

- [ ] **Step 5: Commit**

```bash
git add crates/umbral-core/src/orm/queryset/mod.rs crates/umbral-core/src/orm/queryset/relation_resolve.rs
git commit -m "refactor(orm): rebuild apply_join_related on walk_joins; delete JoinHop/resolve_join_hops/resolve_m2m_chain"
```

---

### Task 5: `select_related` errors loudly on an unresolved path (behavior change)

**Files:**
- Modify: `crates/umbral-core/src/orm/queryset/mod.rs` (the `select_related` resolution path — where the old code returned `None`/skipped)
- Test: `crates/umbral-core/tests/select_related_loud_error.rs` (new)

**Interfaces:**
- Consumes: `RelPath::from_path` returning `Err` on a bad path.
- Produces: a `select_related("<bad>")` followed by a terminal returns `Err` naming the bad relation, instead of silently issuing an un-joined (N+1) query.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn select_related_unknown_relation_is_err_not_silent() {
    // App setup registering Post/User … + one real Post row
    let res = Post::objects().select_related("athor").first().await; // typo
    let err = res.expect_err("must be a loud error, not a silent skip");
    assert!(err.to_string().contains("athor"), "names the bad relation: {err}");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbral-core --test select_related_loud_error`
Expected: FAIL (today it silently skips → `Ok(Some(post))`).

- [ ] **Step 3: Implement** — in the `select_related` resolution, propagate `RelPath::from_path::<T>(field).map_err(...)?` (or record the error and surface it at the terminal via the existing `poisoned` mechanism the traversal QuerySet uses). Match the existing loud message style used by `join_related` (`"umbral::orm::join_related: unknown field ..."`).

- [ ] **Step 4: Run test to verify it passes** + confirm no regression

Run: `cargo test -p umbral-core --test select_related_loud_error` then `cargo test -p umbral-core`.
Expected: new test PASS; a search for any existing test that *relied* on silent-skip (there should be none — `join_related` already errored) shows none broken.

- [ ] **Step 5: Commit**

```bash
git add crates/umbral-core/src/orm/queryset/mod.rs crates/umbral-core/tests/select_related_loud_error.rs
git commit -m "feat(orm)!: select_related errors loudly on an unknown relation (was silent-skip)"
```

---

### Task 6: Doc page + full-workspace verification

**Files:**
- Create/modify: `documentation/docs/v0.0.1/orm/select-related.mdx`

**Interfaces:** none (docs).

- [ ] **Step 1: Write the doc** — purpose (one paragraph: `select_related` is now a pure performance hint that JOIN-hydrates FKs in one query, and a typo'd relation is a compile-safe-ish loud runtime error), one example, and a `<Callout>` noting the behavior change (silent-skip → error). Link to `docs/specs/orm-heavy-relations-epic.md`. Frontmatter: `title`, `description`, `sidebar_position`. Prose not hard-wrapped.

- [ ] **Step 2: Full workspace gate**

Run: `cargo fmt && cargo clippy --all-targets && cargo build && cargo test`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add documentation/docs/v0.0.1/orm/select-related.mdx
git commit -m "docs(orm): select_related loud-error + unified JOIN engine note"
```

---

## Self-Review

- **Spec coverage:** canonical `RelPath` (Task 1–2), one `walk_joins` (Task 3), rebuilt callers + deleted old builders (Task 3–4), loud errors (Task 5), doc (Task 6). All of sub-project A's "Design" and "Files (A)" bullets map to a task. ✓
- **Placeholder scan:** the `/* … static */` markers in Task 2 are flagged *in-task* with a mandatory decision (Cow vs interner) and are not deferrable — the implementer resolves them in Task 2 Step 3, not later. ✓
- **Type consistency:** `walk_joins` signature is identical in Task 3 (produces) and Task 4 (consumes); `RelPath::from_path` signature identical in Task 2/4/5; `NullJoinPolicy`/`PathBase::TableRoot` from Task 1 used unchanged downstream. ✓
- **Regression discipline:** Tasks 3 & 4 run the existing suites as the net before and after; the only sanctioned behavior change is Task 5's loud error. ✓
