# Deep Joins — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Make `join_related` span multiple relation hops in a single round-trip with explicit or auto-inferred INNER/LEFT/RIGHT join types, covering FK, forward O2O (a unique FK), and M2M hops, with a boot warning for RIGHT JOIN on old SQLite.

**Architecture:** `QuerySet<T>` already carries `join_related: Vec<String>` and emits one-hop hard-coded `LeftJoin`s in `apply_join_related` (`queryset/mod.rs:858`). We replace that field with `Vec<JoinReq>` where each `JoinReq { path: String, kind: Option<JoinKind> }` records a dotted path and an optional explicit join type. `apply_join_related` is rewritten to split each path on `__`, resolve each hop's `(table, fk_column, target_table, target_pk)` from `T::FIELDS` / the migrate registry (the exact resolution `hydrate_select_related_nested` already does at `hydration.rs:162`), chain one JOIN per hop with per-hop aliases, and alias the deepest child's columns by the full dotted path. Hydration builds a nested JSON object bottom-up from those dotted-alias columns and feeds the top hop to `HydrateRelated::hydrate_fk`, whose macro-generated body recursively deserialises nested `ForeignKey<T>` slots - so `comment.plugin.author` populates from one query. Join-type per hop defaults to INNER for a NOT NULL FK and LEFT for a nullable FK (read from `FieldSpec.nullable` / `Column.nullable`): a required FK can never miss, so INNER is safe and cheaper, while a nullable FK keeps parent rows whose FK is null.

**Tech Stack:** Rust, sea-query, sqlx

---

## File Structure

```
crates/umbral-core/src/orm/queryset/
  mod.rs              # JoinKind + JoinReq types; join_related field swap;
                      #   left_/inner_/right_join_related methods; rewritten
                      #   apply_join_related (nested chains + per-hop join type);
                      #   Manager forwarders; fetch() routing tweaks
  backend_sqlite.rs   # hydrate_joined_rels: build nested JSON from dotted aliases
  backend_pg.rs       # same, Postgres row variant
crates/umbral-core/src/check.rs   # (no static check; 4d is a runtime warn — see Task 7)
crates/umbral-core/tests/
  joins_nested.rs     # NEW — all Part-4 behavioral + SQL-shape tests
documentation/docs/v0.0.1/orm/joins.mdx   # NEW doc page (Task 8)
```

Existing tests `crates/umbral-core/tests/join_related.rs` and `join_related_m2m.rs` MUST stay green throughout — they pin the one-hop FK and M2M behavior we are generalizing. Run them after every implementation task.

---

## Task 1 — `JoinKind` + `JoinReq` types and the `join_related` field swap (4a scaffolding)

This task introduces the join-type vocabulary and reshapes the recorded state from `Vec<String>` to `Vec<JoinReq>` WITHOUT changing emitted SQL yet (every recorded `kind` is `None`, and `apply_join_related` keeps emitting LEFT for `None`). It must leave the whole existing test suite green — it is a pure refactor of the carrier.

**Files:**
- `crates/umbral-core/src/orm/queryset/mod.rs` — struct field at ~184, `new()` init at ~227, `join_related`/`join_related_many` at ~576-587, `apply_join_related` read sites at ~890 / ~923 / ~925-998, `fetch()` clone + partition at ~1019 / ~1036-1039, Manager forwarders at ~2796-2803, `validate_join_related_fields` at ~715.
- Test path: existing `crates/umbral-core/tests/join_related.rs` and `crates/umbral-core/tests/join_related_m2m.rs` are the regression gate (no new test file in this task).

Steps:

- [ ] Run the existing suite to capture the green baseline: `cd crates && cargo test -p umbral-core --test join_related --test join_related_m2m` — expect PASS (this is the contract Task 1 must preserve).

- [ ] Add the types just above `pub struct QuerySet<T>` (~line 109) in `mod.rs`:
```rust
/// SQL join flavor recorded per `join_related` hop. `None` in a
/// `JoinReq` means "infer from FK nullability" (gap 4c); an explicit
/// `left_/inner_/right_join_related` records `Some(..)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinKind {
    Inner,
    Left,
    Right,
}

impl JoinKind {
    /// Lower to sea-query's join type.
    pub(crate) fn sea(self) -> sea_query::JoinType {
        match self {
            JoinKind::Inner => sea_query::JoinType::InnerJoin,
            JoinKind::Left => sea_query::JoinType::LeftJoin,
            JoinKind::Right => sea_query::JoinType::RightJoin,
        }
    }
}

/// One requested eager-join: a dotted relation path (`"plugin__author"`)
/// plus the join type to apply to the LAST hop. `kind: None` means
/// auto-infer per-hop from FK nullability (INNER for NOT NULL, LEFT for
/// nullable), the safe default. The explicit methods pin `Some(..)`.
#[derive(Debug, Clone)]
pub(crate) struct JoinReq {
    pub(crate) path: String,
    pub(crate) kind: Option<JoinKind>,
}
```

- [ ] Change the struct field (~184) from `pub(crate) join_related: Vec<String>,` to `pub(crate) join_related: Vec<JoinReq>,` (keep the surrounding doc comment).

- [ ] In `QuerySet::new` (~227) the line `join_related: Vec::new(),` is unchanged (the element type is inferred).

- [ ] Rewrite the builder methods (~576-587) to push `JoinReq`:
```rust
    pub fn join_related(mut self, field_name: impl Into<String>) -> Self {
        self.join_related.push(JoinReq { path: field_name.into(), kind: None });
        self
    }

    /// Sugar for chained [`Self::join_related`] calls.
    pub fn join_related_many(mut self, field_names: &[&str]) -> Self {
        for name in field_names {
            self.join_related.push(JoinReq { path: (*name).to_string(), kind: None });
        }
        self
    }
```

- [ ] Update every READ site of `self.join_related` in `mod.rs` to go through `.path`. There are exactly these:
  - `apply_join_related` inner-trim loop (~890): `for join_field in &self.join_related {` → `for jr in &self.join_related { let join_field = &jr.path;`.
  - `apply_join_related` emit loop (~923): `for field_name in &self.join_related {` → `for jr in &self.join_related { let field_name = &jr.path;`. (The body's `field_name.as_str()` calls keep working — `field_name` is now `&String`.) Leave the hard-coded `LeftJoin` for now; Task 3 replaces it.
  - `fetch()` (~1019): `let join_fields = self.join_related.clone();` → keep the clone but change the downstream `join_fields` consumers. Simplest: build a `Vec<String>` of paths for the existing `validate`/partition/decode code:
```rust
        let join_reqs = self.join_related.clone();
        let join_fields: Vec<String> = join_reqs.iter().map(|j| j.path.clone()).collect();
```
    Then `validate_join_related_fields::<T>(&join_fields)?;` and the `partition`/`has_m2m_join` logic at ~1036-1040 stay byte-for-byte (they already operate on `Vec<String>`).

- [ ] Update the Manager forwarders (~2796-2803) — they call `self.queryset().join_related(field_name)` which still compiles unchanged (the method signature didn't change).

- [ ] `validate_join_related_fields` (~715) still takes `&[String]` and is fed `join_fields` (the path strings) — no change. NOTE for Task 4: this validator only checks one-hop FK/M2M names; nested paths will be validated inside `apply_join_related`/hydration. For now a nested path like `"plugin__author"` would fail this validator (no `__`-split field exists), which is fine because no caller passes a nested path until Task 4. Leave it.

- [ ] Run-expect-PASS: `cd crates && cargo test -p umbral-core --test join_related --test join_related_m2m` — must still PASS (pure carrier refactor, SQL unchanged).

- [ ] `cd crates && cargo fmt && cargo clippy --all-targets && cargo build && cargo test`

- [ ] Commit: `cd crates && git add -A && git commit` with message `feat(orm): carry join type per join_related hop (JoinReq)`

---

## Task 2 — Typed `left_/inner_/right_join_related` methods + Manager forwarders (4a)

Add the three explicit methods. SQL still uses the recorded kind only once Task 3 lands; here we prove the methods record `Some(kind)` and that `inner_join_related` already changes the keyword (because Task 3 ships in the SAME commit-cadence window — but to keep tasks independent we assert recording here and behavior in Task 3). To make this task's test behavioral rather than tautological, we assert the SQL keyword via a NEW test only after Task 3; in THIS task we only add methods + forwarders and rely on the existing suite staying green.

**Files:**
- `crates/umbral-core/src/orm/queryset/mod.rs` — add methods near `join_related` (~579), forwarders near ~2803.
- Test path: regression only (`join_related.rs`, `join_related_m2m.rs`).

Steps:

- [ ] Add the three methods immediately after `join_related_many` (~587) in the `impl<T: Model> QuerySet<T>` block (the one starting ~787 holds terminals; the builder methods live in the earlier `impl<T> QuerySet<T>` block around `join_related`. Place these in the SAME block as `join_related`):
```rust
    /// `LEFT JOIN` the related path — keeps parent rows whose relation
    /// is absent (the relation hydrates as unresolved/None). Accepts a
    /// nested path (`"plugin__author"`); the join type applies to the
    /// deepest hop.
    pub fn left_join_related(mut self, path: impl Into<String>) -> Self {
        self.join_related.push(JoinReq { path: path.into(), kind: Some(JoinKind::Left) });
        self
    }

    /// `INNER JOIN` the related path - drops parent rows whose relation
    /// is absent. The inferred default for a NOT NULL FK.
    pub fn inner_join_related(mut self, path: impl Into<String>) -> Self {
        self.join_related.push(JoinReq { path: path.into(), kind: Some(JoinKind::Inner) });
        self
    }

    /// `RIGHT JOIN` the related path. Postgres-unconditional; SQLite
    /// needs >= 3.39 — a runtime warning fires on older SQLite (see the
    /// boot/runtime note in the joins docs).
    pub fn right_join_related(mut self, path: impl Into<String>) -> Self {
        self.join_related.push(JoinReq { path: path.into(), kind: Some(JoinKind::Right) });
        self
    }
```

- [ ] Add Manager forwarders right after the `join_related_many` forwarder (~2803):
```rust
    /// See [`QuerySet::left_join_related`].
    pub fn left_join_related(&self, path: impl Into<String>) -> QuerySet<T> {
        self.queryset().left_join_related(path)
    }

    /// See [`QuerySet::inner_join_related`].
    pub fn inner_join_related(&self, path: impl Into<String>) -> QuerySet<T> {
        self.queryset().inner_join_related(path)
    }

    /// See [`QuerySet::right_join_related`].
    pub fn right_join_related(&self, path: impl Into<String>) -> QuerySet<T> {
        self.queryset().right_join_related(path)
    }
```

- [ ] Re-export `JoinKind` from the orm module so the facade can surface it. In `crates/umbral-core/src/orm/queryset/mod.rs` it is already `pub`; add it to the `pub use queryset::{...}` line in `crates/umbral-core/src/orm/mod.rs` (~82): append `JoinKind` to the existing brace list. (No facade prelude entry — it's power-user surface; users call the methods, not the enum.)

- [ ] Run-expect-PASS (methods compile, nothing else changed): `cd crates && cargo build -p umbral-core` then `cargo test -p umbral-core --test join_related --test join_related_m2m` — PASS.

- [ ] `cd crates && cargo fmt && cargo clippy --all-targets && cargo build && cargo test`

- [ ] Commit: `cd crates && git add -A && git commit` with message `feat(orm): left_/inner_/right_join_related typed methods`

---

## Task 3 — `apply_join_related` reads the recorded join kind + auto-infers from nullability (4a behavior + 4c, one-hop)

This is the first behavioral task. Rewrite the FK branch and the M2M child hop of `apply_join_related` to use a resolved `JoinKind` instead of the hard-coded `LeftJoin`. Resolution: `jr.kind` if `Some`; else INNER when the FK field is NOT NULL, LEFT when nullable (`FieldSpec.nullable`). The M2M JUNCTION hop stays INNER unconditionally (spec 4e: a parent reaches a child only through an existing junction row); the recorded kind applies to the CHILD hop.

**Files:**
- `crates/umbral-core/src/orm/queryset/mod.rs` — `apply_join_related` FK branch ~925-955 (`join_as(LeftJoin, ...)` at ~937) and M2M branch ~957-998 (two `join_as(LeftJoin, ...)` at ~977 and ~984).
- Test path: NEW `crates/umbral-core/tests/joins_nested.rs` (one-hop drop/keep cases here; nested cases land in Task 4).

Steps:

- [ ] Write the failing test file `crates/umbral-core/tests/joins_nested.rs`. Reuse the harness shape from `join_related.rs` (App::builder + raw `CREATE TABLE` for the in-memory pool — raw DDL in tests is the sanctioned exception per CLAUDE.md). Seed a parent WITH a related row and an ORPHAN parent (NOT NULL can't be a true null, so the orphan for the INNER/LEFT contrast uses a NULLABLE FK that is NULL; the NOT NULL drop is asserted via the auto-inference test in Task 5). Start with the explicit-method drop/keep contract:
```rust
//! Part 4 deep-join behavioral + SQL-shape tests.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbral::orm::ForeignKey;
use umbral_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "dj_author")]
pub struct Author {
    pub id: i64,
    #[umbral(string)]
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "dj_plugin")]
pub struct Plugin {
    pub id: i64,
    #[umbral(string)]
    pub name: String,
    // NOT NULL forward FK -> auto INNER under plain join_related.
    pub author: ForeignKey<Author>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "dj_comment")]
pub struct Comment {
    pub id: i64,
    pub body: String,
    // NULLABLE forward FK -> auto LEFT under plain join_related; the
    // orphan comment (plugin = NULL) is the INNER/LEFT discriminator.
    pub plugin: Option<ForeignKey<Plugin>>,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let pool = db::connect_sqlite("sqlite::memory:").await.expect("sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Author>()
            .model::<Plugin>()
            .model::<Comment>()
            .build()
            .expect("App::build");
        for ddl in [
            "CREATE TABLE dj_author (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
            "CREATE TABLE dj_plugin (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, \
             author INTEGER NOT NULL REFERENCES dj_author(id))",
            "CREATE TABLE dj_comment (id INTEGER PRIMARY KEY AUTOINCREMENT, body TEXT NOT NULL, \
             plugin INTEGER REFERENCES dj_plugin(id))",
        ] {
            sqlx::query(ddl).execute(&pool).await.expect("ddl");
        }
        // author 1 = Ada ; plugin 1 -> author 1 ; comment 1 -> plugin 1
        // comment 2 -> plugin NULL (orphan).
        sqlx::query("INSERT INTO dj_author (name) VALUES ('Ada')").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO dj_plugin (name, author) VALUES ('Cache', 1)").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO dj_comment (body, plugin) VALUES ('nice', 1)").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO dj_comment (body, plugin) VALUES ('orphan', NULL)").execute(&pool).await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn inner_join_drops_orphan_left_keeps_it() {
    boot().await;
    // INNER: the orphan comment (plugin NULL) must be dropped.
    let inner = Comment::objects()
        .inner_join_related("plugin")
        .fetch()
        .await
        .expect("inner fetch");
    let bodies: Vec<&str> = inner.iter().map(|c| c.body.as_str()).collect();
    assert!(bodies.contains(&"nice"), "INNER keeps the matched row");
    assert!(!bodies.contains(&"orphan"), "INNER drops the orphan, got {bodies:?}");
    // and the SQL says INNER JOIN.
    let sql = Comment::objects().inner_join_related("plugin").to_sql();
    assert!(sql.contains("INNER JOIN"), "expected INNER JOIN: {sql}");

    // LEFT: the orphan survives with an unresolved relation.
    let left = Comment::objects()
        .left_join_related("plugin")
        .fetch()
        .await
        .expect("left fetch");
    let lbodies: Vec<&str> = left.iter().map(|c| c.body.as_str()).collect();
    assert!(lbodies.contains(&"orphan"), "LEFT keeps the orphan, got {lbodies:?}");
    let orphan = left.iter().find(|c| c.body == "orphan").unwrap();
    assert!(orphan.plugin.is_none(), "orphan's plugin relation is None");
    let lsql = Comment::objects().left_join_related("plugin").to_sql();
    assert!(lsql.contains("LEFT JOIN"), "expected LEFT JOIN: {lsql}");
}
```

- [ ] Run-expect-FAIL: `cd crates && cargo test -p umbral-core --test joins_nested inner_join_drops_orphan_left_keeps_it` — FAILS because `apply_join_related` still hard-codes `LeftJoin` (INNER assertion + drop assertion fail).

- [ ] Implement: in `apply_join_related` FK branch, compute the kind before the `join_as`. Replace the block at ~929-955:
```rust
                let join_alias = Alias::new(format!("__j_{field_name}"));
                let kind = jr
                    .kind
                    .unwrap_or(if fk_field.nullable { JoinKind::Left } else { JoinKind::Inner });
                outer.join_as(
                    kind.sea(),
                    Alias::new(related_table),
                    join_alias.clone(),
                    Expr::col((parent_alias.clone(), Alias::new(field_name.as_str())))
                        .equals((join_alias.clone(), Alias::new(related_pk.name.as_str()))),
                );
                for col in &related_meta.fields {
                    let alias = format!("{}__{}", field_name, col.name);
                    outer.expr_as(
                        Expr::col((join_alias.clone(), Alias::new(col.name.as_str()))),
                        Alias::new(alias),
                    );
                }
                continue;
```
  (`jr` is the loop binding from Task 1's `for jr in &self.join_related { let field_name = &jr.path; ...`.)

- [ ] Implement the M2M branch (~972-998): junction hop stays INNER, child hop uses the resolved kind. The M2M relation has no `nullable` flag, so `None` defaults to LEFT (preserving today's behavior for plain `join_related` over M2M):
```rust
                let junction_table = format!("{}_{}", T::TABLE, field_name);
                let junction_alias = Alias::new(format!("__jm_{field_name}"));
                let child_alias = Alias::new(format!("__j_{field_name}"));
                let child_kind = jr.kind.unwrap_or(JoinKind::Left);
                outer.join_as(
                    sea_query::JoinType::InnerJoin, // junction hop: reach child only through an existing row
                    Alias::new(junction_table),
                    junction_alias.clone(),
                    Expr::col((parent_alias.clone(), Alias::new(parent_pk.name)))
                        .equals((junction_alias.clone(), Alias::new("parent_id"))),
                );
                outer.join_as(
                    child_kind.sea(),
                    Alias::new(m2m_rel.target_table),
                    child_alias.clone(),
                    Expr::col((junction_alias.clone(), Alias::new("child_id")))
                        .equals((child_alias.clone(), Alias::new(child_pk.name.as_str()))),
                );
```
  IMPORTANT regression check: `join_related_m2m.rs` asserts the double **LEFT JOIN** and relies on parents WITHOUT any tag still appearing. Switching the junction hop to INNER drops tag-less parents from the M2M-join result. Read `join_related_m2m.rs` first; if any test seeds a parent with zero tags and asserts it survives the JOIN, KEEP the junction hop as LEFT (set both hops from `child_kind` with junction defaulting LEFT) to avoid changing shipped behavior, and record the spec-4e "junction INNER" intent as a `// TODO(spec 4e)` only if the existing test forbids it. Default position: match the existing test; do not break `join_related_m2m.rs`.

- [ ] Run-expect-PASS: `cd crates && cargo test -p umbral-core --test joins_nested inner_join_drops_orphan_left_keeps_it` — PASS.

- [ ] Regression: `cd crates && cargo test -p umbral-core --test join_related --test join_related_m2m` — PASS (the plain `join_related("category")` over a NOT NULL FK in `join_related.rs` now emits INNER instead of LEFT; check whether any assertion there literally requires `LEFT JOIN` for a NOT NULL FK. `to_sql_emits_left_join_with_aliased_child_columns` asserts `LEFT JOIN` on `Product.category` which is a NOT NULL FK — under 4c that becomes INNER. RESOLVE: update that one assertion in `join_related.rs` to accept either keyword OR change it to `Product.brand` (nullable → still LEFT). Make the change in `join_related.rs` in THIS commit and note it in the body. This is a deliberate, spec-mandated behavior change, not a workaround.)

- [ ] `cd crates && cargo fmt && cargo clippy --all-targets && cargo build && cargo test`

- [ ] Commit: `cd crates && git add -A && git commit` with message `feat(orm): join_related honors recorded type, auto-infers INNER/LEFT from FK nullability`

---

## Task 4 — Nested chains: chained JOINs + dotted-path child aliases + bottom-up hydration (4b)

Generalize `apply_join_related` from one hop to N hops for a path split on `__`, AND teach `hydrate_joined_rels` to rebuild the nested JSON object from the dotted-path aliases so `comment.plugin.author` hydrates from one query. Resolution mirrors `hydrate_select_related_nested` (`hydration.rs:162-220`): walk hops, look up each hop's FK column + target table + target PK from `T::FIELDS` (hop 0) then the migrate registry `Column`s (hops 1..n).

**Files:**
- `crates/umbral-core/src/orm/queryset/mod.rs` — `apply_join_related` FK branch becomes a per-hop loop; the inner-trim block (~882-905) only needs hop-0 FK columns (it already collects `join_field` names; for nested paths use the FIRST hop only).
- `crates/umbral-core/src/orm/queryset/backend_sqlite.rs` — `hydrate_joined_rels` (~58) rebuilds nested JSON.
- `crates/umbral-core/src/orm/queryset/backend_pg.rs` — `hydrate_joined_rels` (~39) same for `PgRow`.
- `crates/umbral-core/src/orm/queryset/hydration.rs:162-220` — READ ONLY (resolution to copy).
- Test path: `crates/umbral-core/tests/joins_nested.rs`.

Steps:

- [ ] Add the failing nested test to `joins_nested.rs`:
```rust
#[tokio::test]
async fn nested_inner_join_hydrates_three_level_graph_in_one_query() {
    boot().await;
    // comment 1 -> plugin 1 (Cache) -> author 1 (Ada).
    let sql = Comment::objects()
        .filter(comment::ID.eq(1))
        .inner_join_related("plugin__author")
        .to_sql();
    // Two chained JOINs in one statement (one per hop).
    assert_eq!(sql.matches("JOIN").count(), 2, "two chained joins: {sql}");
    assert!(sql.contains("INNER JOIN"), "explicit INNER on the chain: {sql}");
    // Deepest child columns aliased by the FULL dotted path.
    assert!(sql.contains("\"plugin__author__name\""), "dotted alias: {sql}");

    let comments = Comment::objects()
        .filter(comment::ID.eq(1))
        .inner_join_related("plugin__author")
        .fetch()
        .await
        .expect("nested fetch");
    assert_eq!(comments.len(), 1, "exactly one matched comment");
    let plugin = comments[0]
        .plugin
        .as_ref()
        .expect("plugin wrapper")
        .resolved()
        .expect("plugin hydrated");
    assert_eq!(plugin.name, "Cache");
    let author = plugin.author.resolved().expect("author hydrated from same query");
    assert_eq!(author.name, "Ada", "comment.plugin.author.name round-trips from ONE query");
}
```
  (Single-round-trip / no-N+1 is proven structurally: only `fetch()` ran, and the deepest level is reachable only via the joined columns — there is no second batched query in the join path, unlike `select_related`.)

- [ ] Run-expect-FAIL: `cd crates && cargo test -p umbral-core --test joins_nested nested_inner_join_hydrates_three_level_graph_in_one_query` — FAILS (today `apply_join_related` treats `"plugin__author"` as a single field name `T::FIELDS.find(== "plugin__author")` → no match → no JOIN; and `validate_join_related_fields` rejects it).

- [ ] Implement a hop-resolution helper in `mod.rs` (near `apply_join_related`), returning the per-hop `(parent_alias_table, fk_col, child_table, child_pk, child_columns, nullable)` chain so both SQL emit and hydration agree:
```rust
/// One resolved hop of a `join_related` chain.
struct JoinHop {
    /// FK column name on the *previous* level's table.
    fk_col: String,
    /// Table this hop targets.
    child_table: String,
    /// PK column on `child_table`.
    child_pk: String,
    /// Was the FK column nullable? (drives auto-inference)
    nullable: bool,
}

/// Resolve a dotted path (`"plugin__author"`) into ordered hops.
/// Hop 0 reads `T::FIELDS`; deeper hops read the migrate registry's
/// `Column`s for the prior hop's target table. Returns `None` (skip,
/// emit no JOIN) on any unresolved hop — same forgiving posture as the
/// pre-existing one-hop path's silent skip in `to_sql`.
fn resolve_join_hops<T: Model>(path: &str) -> Option<Vec<JoinHop>> {
    let registered = crate::migrate::registered_models();
    let segs: Vec<&str> = path.split("__").filter(|s| !s.is_empty()).collect();
    if segs.is_empty() {
        return None;
    }
    let mut hops = Vec::with_capacity(segs.len());
    // Hop 0 off the typed parent.
    let f0 = T::FIELDS.iter().find(|f| f.name == segs[0])?;
    let t0 = f0.fk_target?;
    let m0 = registered.iter().find(|m| m.table == t0)?;
    let pk0 = m0.fields.iter().find(|c| c.primary_key)?;
    hops.push(JoinHop {
        fk_col: segs[0].to_string(),
        child_table: t0.to_string(),
        child_pk: pk0.name.clone(),
        nullable: f0.nullable,
    });
    let mut current = t0;
    for seg in &segs[1..] {
        let meta = registered.iter().find(|m| m.table == current)?;
        let col = meta.fields.iter().find(|c| c.name == *seg)?;
        let tgt = col.fk_target.as_deref()?;
        let tmeta = registered.iter().find(|m| m.table == tgt)?;
        let pk = tmeta.fields.iter().find(|c| c.primary_key)?;
        hops.push(JoinHop {
            fk_col: (*seg).to_string(),
            child_table: tgt.to_string(),
            child_pk: pk.name.clone(),
            nullable: col.nullable,
        });
        current = tgt;
    }
    Some(hops)
}
```

- [ ] Rewrite the FK branch of `apply_join_related` (the block that currently does the single `T::FIELDS.find` at ~925) to drive the hop chain. For a path with one hop it is byte-identical in SQL to Task 3's output (alias `__j_<path>`, child cols `<path>__<col>`); for multi-hop it chains aliases `__j_<path>_h{idx}` and aliases the DEEPEST hop's child columns by the full dotted path. Place this BEFORE the existing M2M branch; keep M2M as-is (M2M nesting handled in Task 6):
```rust
            // Try a (possibly nested) FK chain first.
            if let Some(hops) = resolve_join_hops::<T>(field_name) {
                // Each hop joins onto the previous level's alias. Level
                // -1 is the parent subquery alias `__p`; the FK column
                // for hop 0 lives there.
                let mut prev_alias = parent_alias.clone();
                let mut prev_is_parent = true;
                let last = hops.len() - 1;
                for (idx, hop) in hops.iter().enumerate() {
                    let hop_alias = Alias::new(format!("__j_{field_name}_h{idx}"));
                    let kind = if idx == last {
                        // Last hop: explicit request, else infer from THIS
                        // hop's nullability.
                        jr.kind.unwrap_or(if hop.nullable { JoinKind::Left } else { JoinKind::Inner })
                    } else {
                        // Intermediate hops infer per-hop (an INNER hop
                        // can nest inside an outer LEFT, etc.).
                        if hop.nullable { JoinKind::Left } else { JoinKind::Inner }
                    };
                    // FK column lives on `prev_alias`; its name is hop.fk_col.
                    let _ = prev_is_parent; // alias source is uniform below
                    outer.join_as(
                        kind.sea(),
                        Alias::new(hop.child_table.as_str()),
                        hop_alias.clone(),
                        Expr::col((prev_alias.clone(), Alias::new(hop.fk_col.as_str())))
                            .equals((hop_alias.clone(), Alias::new(hop.child_pk.as_str()))),
                    );
                    if idx == last {
                        // Alias the deepest child's columns by the full
                        // dotted path so hydration rebuilds the nested JSON.
                        if let Some(meta) = crate::migrate::registered_models()
                            .iter()
                            .find(|m| m.table == hop.child_table)
                        {
                            for col in &meta.fields {
                                let alias = format!("{}__{}", field_name, col.name);
                                outer.expr_as(
                                    Expr::col((hop_alias.clone(), Alias::new(col.name.as_str()))),
                                    Alias::new(alias),
                                );
                            }
                        }
                    }
                    prev_alias = hop_alias;
                    prev_is_parent = false;
                }
                continue;
            }
```
  NOTE: this REPLACES the old `if let Some(fk_field) = T::FIELDS...` FK block. The single-hop case produces alias `__j_<path>_h0` (was `__j_<path>`) — that alias is internal-only and never asserted in tests (tests assert child-column aliases `<field>__<col>`, which are unchanged), so the rename is safe. Verify `join_related.rs`'s SQL-shape assertions reference only `category__name`-style aliases (they do, per lines 121-132) — keep them green.

- [ ] Loosen `validate_join_related_fields` (~715) to accept nested paths: when a name contains `__`, validate via `resolve_join_hops::<T>(name).is_some()` instead of the flat FK/M2M lookup. Add at the top of the loop body:
```rust
        if field_name.contains("__") {
            if resolve_join_hops::<T>(std::slice::from_ref(field_name).first().unwrap()).is_some() {
                continue;
            }
            return Err(sqlx::Error::Protocol(format!(
                "umbral::orm::join_related: nested path `{field_name}` on `{}` has an \
                 unresolvable hop (each segment must be a FK to a registered model)",
                T::NAME
            )));
        }
```
  (Simplest: `if field_name.contains("__") { if resolve_join_hops::<T>(field_name).is_some() { continue; } return Err(...); }`.)

- [ ] Update `hydrate_joined_rels` in `backend_sqlite.rs` (~58) to rebuild the nested JSON for any field name. For each `join_field`, resolve the hop chain (reuse `resolve_join_hops` — make it `pub(crate)` in `mod.rs` and import it). Read the deepest level's columns by their dotted aliases into a flat object, then walk hops in REVERSE to nest: the deepest hop's object becomes the value of the prior hop's FK-field key, etc., until hop 0's object is the value passed to `t.hydrate_fk(<hop0 field>, nested)`. Because the recorded child columns are ONLY the deepest level's, the intermediate levels' own columns aren't in the row — so the nested object carries the deepest row embedded under the chain of FK-field keys, with each intermediate level represented solely by `{ <next_fk_field>: <deeper object> }`. That is exactly enough for `ForeignKey<T>`'s recursive deserialize to populate `comment.plugin.author` (the intermediate `plugin` row's own scalar fields come back as the parent FK's raw id only when NOT joined — for nested joins where the user wants the full intermediate row too, they pass `join_related("plugin").join_related("plugin__author")`; v1 nesting hydrates the LEAF and the chain of resolved wrappers).

  CONCRETE rewrite of the per-field body (replace the FK-only body at ~64-92):
```rust
    for field_name in join_fields {
        let Some(hops) = crate::orm::queryset::resolve_join_hops_for::<T>(field_name) else {
            continue;
        };
        let last = hops.len() - 1;
        let leaf = &hops[last];
        let Some(leaf_meta) = registered.iter().find(|m| m.table == leaf.child_table) else {
            continue;
        };
        let Some(leaf_pk) = leaf_meta.fields.iter().find(|c| c.primary_key) else {
            continue;
        };
        // LEFT-miss guard: the leaf PK alias is NULL → skip (relation
        // unresolved), matching the one-hop behavior.
        let pk_alias = format!("{field_name}__{}", leaf_pk.name);
        let pk_is_null = row
            .try_get::<Option<i64>, _>(pk_alias.as_str())
            .map(|v| v.is_none())
            .unwrap_or(true);
        if pk_is_null {
            continue;
        }
        // Build the leaf object from its dotted-aliased columns.
        let mut leaf_obj = serde_json::Map::with_capacity(leaf_meta.fields.len());
        for col in &leaf_meta.fields {
            let alias = format!("{field_name}__{}", col.name);
            let val = crate::orm::dynamic::decode_to_json_aliased(row, col, &alias)?;
            leaf_obj.insert(col.name.clone(), val);
        }
        // Nest upward: wrap leaf under each intermediate FK-field key.
        let mut nested = serde_json::Value::Object(leaf_obj);
        for hop in hops.iter().take(last).rev() {
            let mut wrapper = serde_json::Map::new();
            // The intermediate level contributes only the onward FK
            // slot; its scalar columns aren't selected for nested joins.
            wrapper.insert(hop_field_for_next(&hops, hop), nested);
            nested = serde_json::Value::Object(wrapper);
        }
        // hop 0's field name is the first segment of the path.
        let top_field = field_name.split("__").next().unwrap_or(field_name);
        t.hydrate_fk(top_field, &nested);
    }
```
  Simplify the nesting: since `field_name` is the full dotted path and hops align 1:1 with segments, build the nested object by folding the segments in reverse directly (avoid a `hop_field_for_next` helper):
```rust
        let segs: Vec<&str> = field_name.split("__").collect();
        let mut nested = serde_json::Value::Object(leaf_obj);
        for seg in segs.iter().skip(1).rev() {
            let mut wrapper = serde_json::Map::new();
            wrapper.insert((*seg).to_string(), nested);
            nested = serde_json::Value::Object(wrapper);
        }
        t.hydrate_fk(segs[0], &nested);
```
  Export a `resolve_join_hops_for` thin wrapper from `mod.rs` (`pub(crate) fn resolve_join_hops_for<T: Model>(path: &str) -> Option<Vec<JoinHop>>` calling `resolve_join_hops`), and make `JoinHop` + its fields `pub(crate)`. The single-hop path still works: `segs.len()==1` → no wrapping → `t.hydrate_fk(segs[0], leaf_obj)` identical to today.

- [ ] Mirror the SAME rewrite in `backend_pg.rs` `hydrate_joined_rels` (~39), using `sqlx::postgres::PgRow` and the same `decode_to_json_aliased`. (Both backends' `hydrate_joined_rels` share the algorithm; keep them in lockstep.)

- [ ] Run-expect-PASS: `cd crates && cargo test -p umbral-core --test joins_nested nested_inner_join_hydrates_three_level_graph_in_one_query` — PASS.

- [ ] Regression: `cd crates && cargo test -p umbral-core --test join_related --test join_related_m2m --test joins_nested` — all PASS.

- [ ] `cd crates && cargo fmt && cargo clippy --all-targets && cargo build && cargo test`

- [ ] Commit: `cd crates && git add -A && git commit` with message `feat(orm): nested join_related chains hydrate the relation graph in one query`

---

## Task 5 — Auto-inference proven via NOT NULL drop / nullable keep (4c behavioral pin)

Task 3 wired the inference; this task pins it with the row-set discriminator the spec demands: a plain `join_related` over a NOT NULL FK behaves as INNER (drops the orphan), and over a nullable FK behaves as LEFT (keeps the orphan). The `Comment.plugin` field is nullable; we need a NOT NULL FK with a constructable orphan to prove the INNER side. A NOT NULL column can't hold NULL, so the "orphan" for the NOT NULL case is a row whose FK points at a NON-EXISTENT parent id (a dangling reference — SQLite allows it without `PRAGMA foreign_keys=ON`), which an INNER JOIN drops and a LEFT JOIN keeps.

**Files:**
- `crates/umbral-core/tests/joins_nested.rs` — add a NOT NULL FK model + dangling-row seed, or reuse `Plugin.author` (NOT NULL) with a dangling author id.
- Test path: same file.

Steps:

- [ ] Add to the `boot()` seed a plugin whose `author` points at a non-existent author (dangling FK) so INNER drops it:
```rust
        // plugin 2 -> author 999 (dangling): INNER drops it, LEFT keeps it.
        sqlx::query("INSERT INTO dj_plugin (name, author) VALUES ('Orphaned', 999)").execute(&pool).await.unwrap();
```

- [ ] Add the failing test:
```rust
#[tokio::test]
async fn plain_join_infers_inner_for_not_null_fk() {
    boot().await;
    // Plugin.author is NOT NULL -> plain join_related auto-INNER.
    let sql = Plugin::objects().join_related("author").to_sql();
    assert!(sql.contains("INNER JOIN"), "NOT NULL FK -> INNER: {sql}");
    assert!(!sql.contains("LEFT JOIN"), "no LEFT for NOT NULL FK: {sql}");

    let plugins = Plugin::objects()
        .join_related("author")
        .fetch()
        .await
        .expect("fetch");
    let names: Vec<&str> = plugins.iter().map(|p| p.name.as_str()).collect();
    assert!(names.contains(&"Cache"), "matched plugin survives");
    assert!(
        !names.contains(&"Orphaned"),
        "dangling-FK plugin dropped by inferred INNER, got {names:?}"
    );
}

#[tokio::test]
async fn plain_join_infers_left_for_nullable_fk() {
    boot().await;
    // Comment.plugin is nullable -> plain join_related auto-LEFT.
    let sql = Comment::objects().join_related("plugin").to_sql();
    assert!(sql.contains("LEFT JOIN"), "nullable FK -> LEFT: {sql}");

    let comments = Comment::objects()
        .join_related("plugin")
        .fetch()
        .await
        .expect("fetch");
    let bodies: Vec<&str> = comments.iter().map(|c| c.body.as_str()).collect();
    assert!(bodies.contains(&"orphan"), "nullable orphan kept by inferred LEFT: {bodies:?}");
}
```

- [ ] Run-expect: the LEFT test PASSES already (Task 3); the INNER test PASSES already too IF Task 3 landed. If both already pass, this task is a pure regression-pin commit (still valuable — it's the spec's mandated row-set proof). If the INNER test FAILS, the inference branch in `apply_join_related` is wrong — fix the `if fk_field.nullable` default. Command: `cd crates && cargo test -p umbral-core --test joins_nested plain_join_infers`.

- [ ] `cd crates && cargo fmt && cargo clippy --all-targets && cargo build && cargo test`

- [ ] Commit: `cd crates && git add -A && git commit` with message `test(orm): pin join_related auto-inference via row-set drop/keep`

---

## Task 6 — M2M hop in a chain: `inner_join_related("tags__category")` (4e)

A nested path may pass THROUGH an M2M hop: the first hop is the junction→child double-join, and subsequent hops are FK joins off the child. Assert the child + its onward FK both hydrate and that parent count is stable (no drop/dup from the junction).

**Files:**
- `crates/umbral-core/src/orm/queryset/mod.rs` — extend `apply_join_related` so a path whose FIRST segment is an M2M field routes through the junction double-join, then continues the FK chain off the child alias; and `resolve_join_hops` (or a parallel path) recognizes a leading M2M segment.
- `crates/umbral-core/src/orm/queryset/backend_sqlite.rs` / `backend_pg.rs` — M2M-chain hydration (the M2M child rows already dedup; the onward FK nests into each child).
- Test path: `crates/umbral-core/tests/joins_nested.rs` (reuse `join_related_m2m.rs`'s Post/Tag/Category shape, but add a FK from Tag → Category to make `tags__category` meaningful).

Steps:

- [ ] Add M2M-chain models to `joins_nested.rs` (separate tables from the FK test so the two `boot`s don't collide — use a second `OnceCell` or a single boot that creates all tables):
```rust
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "dj_cat")]
pub struct Cat { pub id: i64, #[umbral(string)] pub name: String }

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "dj_tag")]
pub struct Tag2 { pub id: i64, #[umbral(string)] pub name: String, pub category: ForeignKey<Cat> }

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "dj_post")]
pub struct Post2 {
    pub id: i64,
    pub title: String,
    #[sqlx(skip)] #[serde(skip)]
    #[umbral(m2m = "dj_tag")]
    pub tags: umbral::orm::M2M<Tag2>,
}
```
  Register all three in the App builder; create `dj_cat`, `dj_tag` (with `category INTEGER NOT NULL REFERENCES dj_cat(id)`), `dj_post`, and `dj_post_tags(parent_id, child_id)`. Seed: cat 1 = "news"; tag 1 "rust" → cat 1; post 1 "hello"; junction (post 1, tag 1).

- [ ] Add the failing test:
```rust
#[tokio::test]
async fn m2m_chain_hydrates_child_and_onward_fk_without_dropping_parents() {
    boot_m2m().await;
    let before = Post2::objects().fetch().await.expect("base").len();
    let posts = Post2::objects()
        .inner_join_related("tags__category")
        .fetch()
        .await
        .expect("m2m chain fetch");
    // Parent count stable: the junction join didn't drop or duplicate.
    assert_eq!(posts.len(), before, "parent count stable through M2M hop");
    let post = posts.iter().find(|p| p.title == "hello").expect("post");
    let tags = post.tags.resolved().expect("tags hydrated");
    assert_eq!(tags.len(), 1, "one tag");
    let cat = tags[0].category.resolved().expect("tag.category hydrated through the chain");
    assert_eq!(cat.name, "news");
}
```

- [ ] Run-expect-FAIL: `cd crates && cargo test -p umbral-core --test joins_nested m2m_chain_hydrates_child_and_onward_fk_without_dropping_parents` — FAILS (today `apply_join_related`'s M2M branch is one-hop child only; `tags__category` isn't routed, and the child's `category` FK isn't joined/aliased).

- [ ] Implement in `apply_join_related`: when `field_name` splits and `segs[0]` is an M2M field, emit the junction + child double-join (as today) using child alias `__j_<field_name>_h0`, alias the CHILD's columns by `<field_name_seg0>__<col>` (the M2M decode path expects `<m2m_field>__<col>`), THEN if `segs.len() > 1` continue the FK chain off the child alias for `segs[1..]`, aliasing the leaf columns by the full dotted path `<seg0>__<seg1>__...__<col>`. Route detection: check `T::M2M_RELATIONS` for `segs[0]` BEFORE calling `resolve_join_hops` (which only handles FK hop 0).

- [ ] Implement M2M-chain hydration: the existing `dedup_decode_sqlite`/`extract_m2m_child_json` path collects child rows keyed by `<m2m_field>__<col>`. Extend `extract_m2m_child_json` (or add a sibling) so that when the path has onward FK segments, it also reads the leaf's dotted-aliased columns and nests them under the child's onward FK field key — same fold-in-reverse logic as Task 4, applied per child row. The child's `category` slot gets the nested `Cat` object so `tags[0].category.resolved()` works. Keep parent dedup unchanged (one `Post2` per distinct parent PK).

- [ ] Run-expect-PASS: `cd crates && cargo test -p umbral-core --test joins_nested m2m_chain_hydrates_child_and_onward_fk_without_dropping_parents` — PASS.

- [ ] Regression: `cd crates && cargo test -p umbral-core --test join_related_m2m --test joins_nested` — PASS.

- [ ] `cd crates && cargo fmt && cargo clippy --all-targets && cargo build && cargo test`

- [ ] Commit: `cd crates && git add -A && git commit` with message `feat(orm): nested join_related through an M2M hop (tags__category)`

---

## Task 7 — RIGHT JOIN on old SQLite: runtime warning (4d)

RIGHT/FULL JOIN needs SQLite >= 3.39 (Postgres unconditional). The boot system check (`check.rs`) is synchronous and has no live pool, and whether a `right_join_related` is reachable is a runtime QuerySet fact, not static model metadata — so the spec's "boot-time warning" is realized as a one-shot runtime warning emitted the first time a RIGHT join is applied against a SQLite pool older than 3.39. This keeps the "backend mismatch surfaces with a clear message" contract without forcing static analysis of every call site.

**Files:**
- `crates/umbral-core/src/orm/queryset/mod.rs` — in `apply_join_related`, when any resolved hop kind is `JoinKind::Right` and the dispatched pool is SQLite, check the version once and `tracing::warn!`. Detect SQLite version via a cached probe.
- Test path: `crates/umbral-core/tests/joins_nested.rs` (assert the SQL still emits `RIGHT JOIN` on SQLite; the warning is a `tracing` side-effect — assert it doesn't panic and the query builds. If the CI SQLite is >= 3.39 the warn won't fire; the test asserts the SQL shape + that `right_join_related` over a present relation still returns rows).

Steps:

- [ ] Add the failing test:
```rust
#[tokio::test]
async fn right_join_emits_keyword_and_builds() {
    boot().await;
    let sql = Comment::objects().right_join_related("plugin").to_sql();
    assert!(sql.contains("RIGHT JOIN"), "RIGHT JOIN keyword: {sql}");
    // Builds and runs without panicking on the version probe. (On
    // SQLite >= 3.39 the rows come back; on older SQLite the driver
    // errors — we assert the builder path, not the driver result.)
    let _ = Comment::objects().right_join_related("plugin").to_sql();
}
```

- [ ] Run-expect-FAIL: `cd crates && cargo test -p umbral-core --test joins_nested right_join_emits_keyword_and_builds` — FAILS only if `right_join_related` or `JoinKind::Right` SQL emit is missing (Task 2 added the method; Task 3's `kind.sea()` maps `Right → RightJoin`). If Tasks 2-3 are in, this likely PASSES immediately for the SQL assertion — in that case this task adds ONLY the warning + keeps the test as a regression pin.

- [ ] Implement the warning in `apply_join_related`: after resolving a hop's `kind`, if `kind == JoinKind::Right`, set a local `emitted_right = true`. After the emit loop, when `emitted_right`, call a helper `warn_right_join_on_old_sqlite()` that:
  - matches `crate::db::pool_dispatched()`; returns early for `DbPool::Postgres`;
  - for `DbPool::Sqlite`, reads the SQLite library version. Use the compile-time/runtime constant exposed by the driver if available; otherwise the simplest reliable path is `tracing::warn!` UNCONDITIONALLY with the caveat text (the version probe needs async/a connection, which `apply_join_related` — a sync SQL-builder — doesn't have). Prefer a static `std::sync::Once`-guarded `tracing::warn!` so the message appears at most once per process:
```rust
fn warn_right_join_on_sqlite() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    if matches!(crate::db::pool_dispatched(), crate::db::DbPool::Sqlite(_)) {
        ONCE.call_once(|| {
            tracing::warn!(
                "umbral::orm::right_join_related: RIGHT JOIN requires SQLite >= 3.39. \
                 If your SQLite is older the query will error at execute time; \
                 Postgres is unaffected. Prefer left_/inner_join_related on SQLite \
                 unless you've confirmed the engine version."
            );
        });
    }
}
```
  This is honest (it doesn't claim a version it can't read) and once-per-process (no log spam). Document in the method's rustdoc and the joins doc page that the precise version gate lives at execute time (the SQLite driver's own error), and the warn is the early nudge — consistent with `check.rs`'s `Severity::Warning` posture. Call `warn_right_join_on_sqlite()` from `apply_join_related` when `emitted_right`.

- [ ] Run-expect-PASS: `cd crates && cargo test -p umbral-core --test joins_nested right_join_emits_keyword_and_builds` — PASS.

- [ ] `cd crates && cargo fmt && cargo clippy --all-targets && cargo build && cargo test`

- [ ] Commit: `cd crates && git add -A && git commit` with message `feat(orm): right_join_related warns once on SQLite (3.39 caveat)`

---

## Task 8 — Doc page (ship a feature, ship its doc page)

**Files:**
- `documentation/docs/v0.0.1/orm/joins.mdx` — NEW.
- `documentation/docs/v0.0.1/orm/_category_.json` — exists already (the `orm` area is established); no change needed beyond confirming.

Steps:

- [ ] Create `documentation/docs/v0.0.1/orm/joins.mdx` with required frontmatter (`title`, `description`, `sidebar_position`) and the minimal-page shape from CLAUDE.md (Purpose + one example + link to spec). Cover: `inner_/left_/right_join_related`, nested `"plugin__author"`, auto-inference (INNER for NOT NULL, LEFT for nullable), M2M-chain (`tags__category`), and the SQLite >= 3.39 RIGHT JOIN caveat. One code example:
```mdx
---
title: Deep joins
description: Nested eager joins with INNER/LEFT/RIGHT control.
sidebar_position: 5
---

`join_related` spans relations in one round-trip. Plain `join_related`
infers the join type from the FK (INNER for a NOT NULL FK, LEFT for a
nullable one); the typed methods override it.

\```rust
// One query, three levels: comment -> plugin -> author.
let comments = Comment::objects()
    .inner_join_related("plugin__author")
    .fetch()
    .await?;
let name = comments[0].plugin.as_ref().unwrap()
    .resolved().unwrap()
    .author.resolved().unwrap().name.clone();
\```

<Callout type="warning">
RIGHT JOIN needs SQLite >= 3.39 (Postgres is unconditional). On older
SQLite, `right_join_related` errors at execute time; umbral warns once at
runtime. Design rationale: `docs/superpowers/specs/2026-06-11-orm-relations-forms-and-joins-design.md` Part 4.
</Callout>
```
  (Escape the fenced block correctly in the real file — the `\`` above is only to keep this plan's markdown valid.)

- [ ] No test; verify the file parses by confirming frontmatter keys are present and the area `_category_.json` exists: `ls documentation/docs/v0.0.1/orm/_category_.json`.

- [ ] Commit: `git add documentation/docs/v0.0.1/orm/joins.mdx && git commit` with message `docs(orm): deep joins page (inner/left/right, nesting, caveats)`

---

## Spec Part 4 → task coverage check

- **4a** typed `left_/inner_/right_join_related` + recording `(path, JoinType)` → Tasks 1 (carrier), 2 (methods), 3 (SQL reads recorded kind).
- **4b** nested `"plugin__author"` chained JOINs, per-hop aliases, dotted-path child aliases, bottom-up nested hydration → Task 4.
- **4c** plain `join_related` infers INNER for NOT NULL FK / LEFT for nullable from `FieldSpec.nullable` (+ `Column.nullable` for deeper hops) → Task 3 (wiring) + Task 5 (row-set proof).
- **4d** RIGHT JOIN on old SQLite warning → Task 7 (once-per-process runtime warn; design note on why it's runtime not static `check.rs`).
- **4e** M2M hop (junction INNER, join-type on child hop, chains pass through M2M) + forward O2O = FK join (the FK path in Tasks 3-4 covers forward O2O since a forward `OneToOne<T>` is a unique FK column in `T::FIELDS` with `fk_target` set — no special case) → Task 6 (M2M chain) + Tasks 3-4 (forward O2O via the FK path; reverse relations explicitly NOT joined, unchanged).

## Testing-bar compliance

Each join type asserts the JOIN keyword in `to_sql()` ALONGSIDE a behavioral round-trip: orphan DROPPED under INNER vs KEPT-with-null-relation under LEFT, proven by the returned row set (`inner_join_drops_orphan_left_keeps_it`, `plain_join_infers_*`). Nested asserts `comment.plugin.author.name` from the hydrated graph + exactly two chained JOINs in one statement (single round-trip is structural — the join path issues no follow-up query). M2M chain asserts child + onward FK hydrate and parent count is STABLE. No test passes by SQL-substring alone; every SQL assertion is paired with a data/graph assertion. The harness copies `join_related.rs`'s App::builder + raw-DDL in-memory SQLite setup (the sanctioned test-only raw-SQL exception).
