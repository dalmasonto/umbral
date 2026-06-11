# annotate_count Hardening — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Make `annotate_count` Django-faithful — exclude soft-deleted children automatically, accept a child-side predicate (`annotate_count_where`), count M2M junction rows, and carry the child's `soft_delete` flag onto `ReverseFkRelationSpec` + `ModelMeta` (the shared #35 enabler).

**Architecture:** The correlated-subquery emission already lives in `QuerySet::build_query_for` (`crates/umbra-core/src/orm/queryset/mod.rs` ~354-378), so every annotation rides the one SELECT through `fetch_annotated` / `explain` / `to_sql`. We extend the `RelatedAnnotation` resolution carried by `annotate_related` to (a) remember whether the child is soft-delete and fold `AND <child>.deleted_at IS NULL` into the subquery, (b) carry an optional child `SimpleExpr` predicate, and (c) recognise M2M relations (fall back to `M2M_RELATIONS`) and emit `SELECT COUNT(*) FROM <parent>_<field> WHERE parent_id = parent.<pk>`. The `soft_delete` bit is surfaced from the typed `Model::SOFT_DELETE` onto `ReverseFkRelationSpec` (filled by the Model derive from the child model) and onto `ModelMeta` (filled by `ModelMeta::for_::<T>()` from `T::SOFT_DELETE`).

**Tech Stack:** Rust, sea-query, sqlx

---

## File Structure

Files this plan touches, with anchors:

- `crates/umbra-core/src/migrate.rs` — `ModelMeta` struct (~250-298) gains `soft_delete: bool`; `ModelMeta::for_::<T>()` (~333-363) fills it from `T::SOFT_DELETE`. The serde default keeps old snapshot JSON round-tripping.
- `crates/umbra-core/src/inspect.rs` — the one *production* `ModelMeta { ... }` struct-literal constructor (~753-765, in `render_initial_migration`) gains `soft_delete: false` (introspected tables have no soft-delete signal).
- **Every other `ModelMeta { ... }` struct-literal** (test fixtures — adding a non-`#[serde(default)]`-less required field would break compilation). Because the new field is `#[serde(default, skip_serializing_if = "is_false")]`, the *struct literals* still must set it. Full list to update (from `grep -rn "ModelMeta {" crates/ --include=*.rs`):
  - `crates/umbra-core/src/inspect.rs:753`
  - `crates/umbra-core/src/orm/validation.rs:574` and `:652`
  - `crates/umbra-core/src/migrate.rs:3596`, `:3612`, `:3955`
  - `crates/umbra-core/tests/dyn_signals.rs:82`
  - `crates/umbra-core/tests/filter_in_strings.rs:69`
  - `crates/umbra-core/tests/dyn_string_pk_include.rs:107`
  - `crates/umbra-core/tests/plugin_contract.rs:104`
  - `crates/umbra-core/tests/dyn_error_enum.rs:56`
  - `crates/umbra-core/tests/dyn_m2m_batched.rs:97`
  - `crates/umbra-core/tests/filter_m2m.rs:109`
  - `crates/umbra-core/tests/dyn_select_related_nested.rs:107`
  - `crates/umbra-core/tests/rename_detection.rs:25`
  - `crates/umbra-core/tests/json_form_parse.rs:66`
  - `crates/umbra-core/tests/migrate.rs:724`, `:1428`
  - (Some of these `grep` hits are helper functions that build *one* `ModelMeta`; each literal in those helpers gets `soft_delete: false`.)
- `crates/umbra-core/src/orm/model.rs` — `ReverseFkRelationSpec` struct (~347-360) gains `soft_delete: bool`.
- `crates/umbra-macros/src/lib.rs` — the `reverse_fk_specs` emission (~955-962) sets `soft_delete: <#inner as ::umbra::orm::Model>::SOFT_DELETE`.
- `crates/umbra-core/src/orm/queryset/mod.rs` — `RelatedAnnotation` (~203-209) gains a `soft_delete: bool` and an `Option<sea_query::SimpleExpr>` child-predicate (plus an M2M-mode marker); `annotate_related` (~2082-2121) records soft-delete; new `annotate_count_where` and M2M fallback in resolution; subquery emission in `build_query_for` (~354-378) folds `deleted_at IS NULL`, the child predicate, and the M2M shape.
- `crates/umbra-core/src/orm/queryset/mod.rs` will also need `use sea_query::Expr;` reachable in `build_query_for` (it already uses `sea_query::Expr` in the soft-delete auto-filter at ~340 via `use sea_query::Expr;` inside the block — confirm/extend).
- `crates/umbra-core/tests/annotate_count.rs` — extend: add a soft-delete child model, a moderation column on the child, an M2M model+junction, and the new behavioral tests.
- `umbra_website/plugins/public/src/lib.rs` (~62-88) — switch `.annotate_count("comment_set")` to `.annotate_count_where::<pd::PluginComment>("comment_set_count_visible"... )` counting visible-only (exact call below).
- `documentation/docs/v0.0.1/orm/aggregates.mdx` — extend with `annotate_count_where` + soft-delete behavior + M2M count.

Reference snippets from the live code (the plan's new code must compile against these):

`ModelMeta::for_` (migrate.rs ~333):
```rust
pub fn for_<T: Model>() -> Self {
    Self {
        name: T::NAME.to_string(),
        table: T::TABLE.to_string(),
        fields: T::FIELDS.iter().map(Column::from).collect(),
        display: T::DISPLAY.to_string(),
        icon: T::ICON.to_string(),
        database: T::DATABASE.map(|s| s.to_string()),
        singleton: T::SINGLETON,
        unique_together: /* ... */,
        indexes: /* ... */,
        ordering: /* ... */,
        m2m_relations: /* ... */,
    }
}
```

`ReverseFkRelationSpec` (model.rs ~347):
```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReverseFkRelationSpec {
    pub field_name: &'static str,
    pub target_table: &'static str,
    pub target_name: &'static str,
    pub fk_column: &'static str,
}
```

`M2MRelationSpec` (model.rs ~303):
```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct M2MRelationSpec {
    pub field_name: &'static str,
    pub target_table: &'static str,
    pub target_name: &'static str,
}
```

`RelatedAnnotation` (queryset/mod.rs ~203):
```rust
#[derive(Debug, Clone)]
pub(crate) struct RelatedAnnotation {
    pub(crate) alias: String,
    pub(crate) agg: crate::orm::Aggregate,
    /// `Ok((child_table, fk_column, parent_table, parent_pk))` or the
    /// loud error message for an unknown relation.
    pub(crate) resolved: Result<(String, String, String, String), String>,
}
```

Subquery emission today (queryset/mod.rs ~354):
```rust
for ann in &self.annotations {
    if let Ok((child_table, fk_col, parent_table, parent_pk)) = &ann.resolved {
        let sub = sea_query::Query::select()
            .expr(ann.agg.to_simple_expr())
            .from(Alias::new(child_table.as_str()))
            .and_where(
                sea_query::Expr::col((
                    Alias::new(child_table.as_str()),
                    Alias::new(fk_col.as_str()),
                ))
                .equals((
                    Alias::new(parent_table.as_str()),
                    Alias::new(parent_pk.as_str()),
                )),
            )
            .to_owned();
        q.expr_as(
            sea_query::SimpleExpr::SubQuery(
                None,
                Box::new(sea_query::SubQueryStatement::SelectStatement(sub)),
            ),
            Alias::new(ann.alias.as_str()),
        );
    }
}
```

`Predicate<T>` renderer (orm/mod.rs ~151):
```rust
pub(crate) fn cond_for(&self, backend_name: &str) -> sea_query::SimpleExpr {
    match backend_name {
        "sqlite" => self.cond_sqlite.clone().unwrap_or_else(|| self.cond.clone()),
        _ => self.cond.clone(),
    }
}
```

M2M junction convention (junction table = `format!("{}_{}", parent_table, field_name)`, columns `parent_id` / `child_id` — confirmed in dynamic.rs ~356-394).

---

## Task 1 — `soft_delete` onto `ModelMeta` + every constructor (green refactor)

**Files:**
- `crates/umbra-core/src/migrate.rs` (struct ~250-298, `for_` ~333-363)
- `crates/umbra-core/src/inspect.rs` (~753-765)
- all test-fixture `ModelMeta { ... }` literals listed in File Structure
- Test: `crates/umbra-core/tests/migrate.rs` (add one assert) + the whole workspace must stay green.

This is a pure plumbing/refactor step. The "test" is the workspace compiling + a small pin that `ModelMeta::for_::<T>()` copies `T::SOFT_DELETE`.

- [ ] **Failing test.** Add to `crates/umbra-core/tests/migrate.rs` (a model with `#[umbra(soft_delete)]` already needs a `deleted_at` column; define a tiny one inline):
  ```rust
  #[test]
  fn model_meta_carries_soft_delete_flag() {
      use serde::{Deserialize, Serialize};

      #[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
      #[umbra(table = "mm_soft", soft_delete)]
      struct SoftThing {
          id: i64,
          name: String,
          #[umbra(index)]
          deleted_at: Option<chrono::DateTime<chrono::Utc>>,
      }

      #[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
      #[umbra(table = "mm_hard")]
      struct HardThing {
          id: i64,
          name: String,
      }

      let soft = umbra::migrate::ModelMeta::for_::<SoftThing>();
      let hard = umbra::migrate::ModelMeta::for_::<HardThing>();
      assert!(soft.soft_delete, "soft_delete model must carry the flag");
      assert!(!hard.soft_delete, "non-soft-delete model must not");
  }
  ```
- [ ] **Run, expect FAIL** (the `soft_delete` field doesn't exist yet → compile error):
  ```
  cd crates && cargo test -p umbra-core --test migrate model_meta_carries_soft_delete_flag
  ```
- [ ] **Implement.** In `crates/umbra-core/src/migrate.rs`, add to the `ModelMeta` struct (after `m2m_relations`, ~297):
  ```rust
  /// Mirrors `Model::SOFT_DELETE` (`#[umbra(soft_delete)]`). The
  /// dynamic / annotate paths read this to auto-exclude
  /// `deleted_at IS NULL` children from correlated counts and to
  /// drive trash-aware admin views without re-reaching into the
  /// typed trait. Shared enabler for gaps2 #35 + #39a.
  #[serde(default, skip_serializing_if = "is_false")]
  pub soft_delete: bool,
  ```
  (`is_false` is the existing serde helper already used by `singleton` — reuse it.) Then in `ModelMeta::for_` add `soft_delete: T::SOFT_DELETE,` to the constructed `Self { ... }`.
- [ ] **Implement — fix every struct literal.** Add `soft_delete: false,` to the `ModelMeta { ... }` literal in `crates/umbra-core/src/inspect.rs` (~753-765, introspected tables carry no soft-delete signal) and to **every** test-fixture literal in the File Structure list. Build will name each one that's missing the field; walk them.
- [ ] **Run, expect PASS** (new test green) and verify the WHOLE workspace builds + tests:
  ```
  cd crates && cargo test -p umbra-core --test migrate model_meta_carries_soft_delete_flag
  cd crates && cargo build && cargo test
  ```
- [ ] **Commit** (only after the entire workspace is green):
  ```
  cd crates && cargo fmt && cargo clippy --all-targets && cargo build && cargo test
  ```
  `feat(orm): carry soft_delete onto ModelMeta (gaps2 #35/#39a enabler)`

---

## Task 2 — `soft_delete` onto `ReverseFkRelationSpec`; Model derive fills it

**Files:**
- `crates/umbra-core/src/orm/model.rs` (`ReverseFkRelationSpec` ~347-360)
- `crates/umbra-macros/src/lib.rs` (`reverse_fk_specs` emission ~955-962)
- Test: `crates/umbra-core/tests/annotate_count.rs` (extend — add a soft-delete child model + a pin on the spec).

- [ ] **Failing test.** In `crates/umbra-core/tests/annotate_count.rs`, add a soft-delete child model alongside `Comment`/`Review` (it must own a `deleted_at` column), then pin the spec flag. Add near the model defs:
  ```rust
  #[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
  #[umbra(table = "anc_note", soft_delete)]
  pub struct Note {
      pub id: i64,
      pub body: String,
      pub post: ForeignKey<Post>,
      #[sqlx(default)]
      #[umbra(index)]
      pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
  }
  ```
  and add a `note_set: ReverseSet<Note>` reverse relation to `Post` (mirroring `comment_set`):
  ```rust
  #[sqlx(skip)]
  #[serde(skip)]
  #[umbra(reverse_fk = "post")]
  pub note_set: ReverseSet<Note>,
  ```
  Then the spec-flag pin:
  ```rust
  #[test]
  fn reverse_fk_spec_carries_child_soft_delete() {
      use umbra::orm::Model;
      let note = Post::REVERSE_FK_RELATIONS
          .iter()
          .find(|r| r.field_name == "note_set")
          .expect("note_set relation");
      assert!(note.soft_delete, "child Note is soft-delete");
      let comment = Post::REVERSE_FK_RELATIONS
          .iter()
          .find(|r| r.field_name == "comment_set")
          .expect("comment_set relation");
      assert!(!comment.soft_delete, "child Comment is not soft-delete");
  }
  ```
  (Register `Note` in `boot()`'s `App::builder()` chain with `.model::<Note>()` and add its `CREATE TABLE anc_note (... deleted_at TEXT ...)`; seed it under Task 3. Adding it now keeps the harness compiling.)
- [ ] **Run, expect FAIL** (no `soft_delete` field on the spec → compile error):
  ```
  cd crates && cargo test -p umbra-core --test annotate_count reverse_fk_spec_carries_child_soft_delete
  ```
- [ ] **Implement — spec field.** In `crates/umbra-core/src/orm/model.rs`, add to `ReverseFkRelationSpec` (after `fk_column`, ~359):
  ```rust
  /// Mirrors the CHILD model's `Model::SOFT_DELETE`. `annotate_count`
  /// folds `AND <child>.deleted_at IS NULL` into the correlated
  /// count subquery when this is `true`, so a trashed child stops
  /// inflating the parent's count. Filled by the Model derive from
  /// `<Child as Model>::SOFT_DELETE`.
  pub soft_delete: bool,
  ```
- [ ] **Implement — derive fills it.** In `crates/umbra-macros/src/lib.rs`, the `reverse_fk_specs.push(quote! { ... })` block (~955-962) becomes:
  ```rust
  reverse_fk_specs.push(quote! {
      ::umbra::orm::ReverseFkRelationSpec {
          field_name: #field_name_str,
          target_table: <#inner as ::umbra::orm::Model>::TABLE,
          target_name: <#inner as ::umbra::orm::Model>::NAME,
          fk_column: #fk_col,
          soft_delete: <#inner as ::umbra::orm::Model>::SOFT_DELETE,
      }
  });
  ```
  (`#inner` is the child type already in scope here.)
- [ ] **Run, expect PASS** + full workspace (every other `ReverseFkRelationSpec { ... }` struct literal in the tree must also gain the field — grep `ReverseFkRelationSpec {` to confirm only the macro emits it; if any hand-written literal exists in tests, fix it):
  ```
  cd crates && cargo test -p umbra-core --test annotate_count reverse_fk_spec_carries_child_soft_delete
  cd crates && cargo build && cargo test
  ```
- [ ] **Commit:**
  ```
  cd crates && cargo fmt && cargo clippy --all-targets && cargo build && cargo test
  ```
  `feat(orm): ReverseFkRelationSpec carries child soft_delete, filled by derive`

---

## Task 3 — `annotate_count` auto-excludes soft-deleted children

**Files:**
- `crates/umbra-core/src/orm/queryset/mod.rs` (`RelatedAnnotation` ~203, `annotate_related` ~2082, subquery emission ~354-378)
- Test: `crates/umbra-core/tests/annotate_count.rs` (behavioral: seed 3 notes, soft-delete 1, expect count 2; zero-note parent still returned as 0).

- [ ] **Failing test.** First extend `boot()` to seed `anc_note` rows. After the comment/review seeds add:
  ```rust
  // alpha (id 1): three notes; we'll soft-delete one in the test.
  for (body, post) in [("n1", 1), ("n2", 1), ("n3", 1)] {
      sqlx::query("INSERT INTO anc_note (body, post) VALUES (?, ?)")
          .bind(body)
          .bind(post)
          .execute(&pool)
          .await
          .expect("seed note");
  }
  ```
  and the `CREATE TABLE anc_note` (in `boot`, register `.model::<Note>()` too):
  ```rust
  sqlx::query(
      "CREATE TABLE anc_note (
          id INTEGER PRIMARY KEY AUTOINCREMENT,
          body TEXT NOT NULL,
          post INTEGER NOT NULL REFERENCES anc_post(id),
          deleted_at TEXT
      )",
  )
  .execute(&pool)
  .await
  .expect("CREATE TABLE anc_note");
  ```
  Then the behavioral test (drives the *real* soft-delete path — `Note::objects().filter(...).delete()` redirects to `UPDATE ... SET deleted_at = NOW()` per Feature #72):
  ```rust
  #[tokio::test]
  async fn annotate_count_excludes_soft_deleted_children() {
      boot().await;
      // Soft-delete exactly one of alpha's three notes via the real
      // soft-delete path (delete() on a soft_delete model UPDATEs
      // deleted_at rather than removing the row).
      let removed = Note::objects()
          .filter(note::BODY.eq("n2"))
          .delete()
          .await
          .expect("soft-delete one note");
      assert_eq!(removed, 1, "exactly one note soft-deleted");

      let rows = Post::objects()
          .annotate_count("note_set")
          .fetch_annotated()
          .await
          .expect("fetch_annotated");
      let by_title: std::collections::HashMap<String, i64> = rows
          .into_iter()
          .map(|(p, a)| (p.title, a["note_set_count"].as_i64().unwrap()))
          .collect();
      assert_eq!(
          by_title["alpha"], 2,
          "soft-deleted note must NOT be counted (3 seeded, 1 trashed)"
      );
      assert_eq!(
          by_title["gamma"], 0,
          "a parent with zero notes is still returned as 0, not dropped"
      );
  }
  ```
- [ ] **Run, expect FAIL** (auto-exclusion not implemented → alpha counts 3):
  ```
  cd crates && cargo test -p umbra-core --test annotate_count annotate_count_excludes_soft_deleted_children
  ```
- [ ] **Implement — carry soft_delete + child predicate on the annotation.** In `crates/umbra-core/src/orm/queryset/mod.rs`, extend `RelatedAnnotation` (~203):
  ```rust
  #[derive(Debug, Clone)]
  pub(crate) struct RelatedAnnotation {
      pub(crate) alias: String,
      pub(crate) agg: crate::orm::Aggregate,
      /// `Ok((child_table, fk_column, parent_table, parent_pk))` or the
      /// loud error message for an unknown relation.
      pub(crate) resolved: Result<(String, String, String, String), String>,
      /// Child model is `#[umbra(soft_delete)]` — fold
      /// `AND <child>.deleted_at IS NULL` into the correlated subquery.
      pub(crate) child_soft_delete: bool,
      /// Optional child-side predicate (Django's `Count(filter=Q(...))`),
      /// pre-rendered to a backend-default `SimpleExpr`. ANDed into the
      /// subquery WHERE. From `annotate_count_where`.
      pub(crate) child_filter: Option<sea_query::SimpleExpr>,
      /// `Some(junction_table)` when this annotation counts M2M
      /// junction rows instead of child rows (Task 5).
      pub(crate) m2m_junction: Option<String>,
  }
  ```
  Update the existing `self.annotations.push(RelatedAnnotation { ... })` in `annotate_related` to set `child_soft_delete`, `child_filter: None`, `m2m_junction: None`. Read the flag during resolution: in the `.map(|spec| { ... })` closure (~2091) the `spec` is the `ReverseFkRelationSpec`, so capture `spec.soft_delete`. Because `resolved` is built inside `.map(...).ok_or_else(...)`, capture the bool *before* mapping into the tuple — restructure to:
  ```rust
  let spec = T::REVERSE_FK_RELATIONS
      .iter()
      .find(|r| r.field_name == relation);
  let child_soft_delete = spec.map(|s| s.soft_delete).unwrap_or(false);
  let resolved = spec
      .map(|spec| {
          let pk = T::FIELDS
              .iter()
              .find(|f| f.primary_key)
              .map(|f| f.name)
              .unwrap_or("id");
          (
              spec.target_table.to_string(),
              spec.fk_column.to_string(),
              T::TABLE.to_string(),
              pk.to_string(),
          )
      })
      .ok_or_else(|| {
          format!(
              "umbra::orm::annotate_related: `{relation}` is not a reverse-FK relation on `{}` — declared relations: [{}]",
              T::NAME,
              T::REVERSE_FK_RELATIONS
                  .iter()
                  .map(|r| r.field_name)
                  .collect::<Vec<_>>()
                  .join(", "),
          )
      });
  self.annotations.push(RelatedAnnotation {
      alias: alias.to_string(),
      agg,
      resolved,
      child_soft_delete,
      child_filter: None,
      m2m_junction: None,
  });
  self
  ```
- [ ] **Implement — fold into the subquery.** In `build_query_for` (~354-378), inside the `if let Ok((child_table, fk_col, parent_table, parent_pk)) = &ann.resolved {` block, after building `sub` with the correlation `and_where`, append the soft-delete and child-filter conditions before `.to_owned()`. Rewrite the body to build `sub` mutably:
  ```rust
  for ann in &self.annotations {
      // M2M-junction annotations (Task 5) take a different shape.
      if let Some(junction) = &ann.m2m_junction {
          if let Ok((_child_table, _fk_col, parent_table, parent_pk)) = &ann.resolved {
              let mut sub = sea_query::Query::select();
              sub.expr(ann.agg.to_simple_expr())
                  .from(Alias::new(junction.as_str()))
                  .and_where(
                      sea_query::Expr::col((
                          Alias::new(junction.as_str()),
                          Alias::new("parent_id"),
                      ))
                      .equals((
                          Alias::new(parent_table.as_str()),
                          Alias::new(parent_pk.as_str()),
                      )),
                  );
              q.expr_as(
                  sea_query::SimpleExpr::SubQuery(
                      None,
                      Box::new(sea_query::SubQueryStatement::SelectStatement(sub.to_owned())),
                  ),
                  Alias::new(ann.alias.as_str()),
              );
          }
          continue;
      }
      if let Ok((child_table, fk_col, parent_table, parent_pk)) = &ann.resolved {
          let mut sub = sea_query::Query::select();
          sub.expr(ann.agg.to_simple_expr())
              .from(Alias::new(child_table.as_str()))
              .and_where(
                  sea_query::Expr::col((
                      Alias::new(child_table.as_str()),
                      Alias::new(fk_col.as_str()),
                  ))
                  .equals((
                      Alias::new(parent_table.as_str()),
                      Alias::new(parent_pk.as_str()),
                  )),
              );
          // 5b — fold the child's soft-delete filter into the count.
          if ann.child_soft_delete {
              sub.and_where(
                  sea_query::Expr::col((
                      Alias::new(child_table.as_str()),
                      Alias::new("deleted_at"),
                  ))
                  .is_null(),
              );
          }
          // 5c — child-side predicate (annotate_count_where).
          if let Some(filter) = &ann.child_filter {
              sub.and_where(filter.clone());
          }
          q.expr_as(
              sea_query::SimpleExpr::SubQuery(
                  None,
                  Box::new(sea_query::SubQueryStatement::SelectStatement(sub.to_owned())),
              ),
              Alias::new(ann.alias.as_str()),
          );
      }
  }
  ```
  (`sea_query::Expr` is already imported in this file's scope; the soft-delete auto-filter above this block uses `use sea_query::Expr;` — keep using the fully-qualified `sea_query::Expr` here for consistency with the existing annotation block.)
- [ ] **Run, expect PASS:**
  ```
  cd crates && cargo test -p umbra-core --test annotate_count annotate_count_excludes_soft_deleted_children
  cd crates && cargo test -p umbra-core --test annotate_count
  ```
  (the existing tests — `counts_arrive_with_the_rows_in_one_query`, `to_sql_and_explain_see_the_annotations`, etc. — must stay green; `comment_set` is not soft-delete so its count is unchanged.)
- [ ] **Commit:**
  ```
  cd crates && cargo fmt && cargo clippy --all-targets && cargo build && cargo test
  ```
  `feat(orm): annotate_count auto-excludes soft-deleted children`

---

## Task 4 — `annotate_count_where` with a child predicate

**Files:**
- `crates/umbra-core/src/orm/queryset/mod.rs` (new method near `annotate_count` ~2127)
- Test: `crates/umbra-core/tests/annotate_count.rs` (mixed visible/hidden children → visible count).

- [ ] **Failing test.** Give the child a moderation column so a predicate has something to filter on. Add a moderation-bearing child to the harness — reuse `Note` by adding a `moderation: String` column (simplest, no enum needed for the test), OR add a dedicated model. Plan uses `Note` extended with a text `moderation` column. Update the `Note` struct:
  ```rust
  pub moderation: String,
  ```
  the `CREATE TABLE anc_note` to add `moderation TEXT NOT NULL DEFAULT 'visible'`, and the seed loop to set moderation per row. Replace the note seed with:
  ```rust
  for (body, post, moderation) in [
      ("n1", 1, "visible"),
      ("n2", 1, "visible"),
      ("n3", 1, "hidden"),
  ] {
      sqlx::query("INSERT INTO anc_note (body, post, moderation) VALUES (?, ?, ?)")
          .bind(body)
          .bind(post)
          .bind(moderation)
          .execute(&pool)
          .await
          .expect("seed note");
  }
  ```
  (Task 3's soft-delete test still soft-deletes `n2` — after that, alpha has visible-not-deleted = `n1` only when both filters apply; keep the two tests independent by not depending on cross-test ordering — each `boot()` call shares the same `OnceCell`, so DO NOT soft-delete in this test. Instead this test asserts the *visible* count from the raw seed: n1+n2 visible = 2, n3 hidden. The soft-delete test mutates state; to keep tests order-independent, the soft-delete test should filter on a body it owns and assert relative to its own mutation. Simplest robust approach: the soft-delete test asserts `>= 2` is wrong — instead, have the soft-delete test target `note_set` and accept that `boot` seeds 3 visible-or-not notes; re-seed dedicated rows. To avoid shared-state coupling, this test uses `annotate_count_where` which is independent of the soft-delete mutation as long as it filters on moderation only.)

  Behavioral test:
  ```rust
  #[tokio::test]
  async fn annotate_count_where_filters_children() {
      boot().await;
      let rows = Post::objects()
          .annotate_count_where::<Note>(
              "visible_notes",
              "note_set",
              note::MODERATION.eq("visible"),
          )
          .fetch_annotated()
          .await
          .expect("fetch_annotated with child filter");
      let alpha = rows
          .iter()
          .find(|(p, _)| p.title == "alpha")
          .expect("alpha row");
      assert_eq!(
          alpha.1["visible_notes"].as_i64(),
          Some(2),
          "only the two visible notes count; the hidden one is excluded"
      );
  }
  ```
  > **Shared-state note for the implementer:** `boot()` uses a process-wide `OnceCell`, so the soft-delete test (Task 3) and this test see the *same* `anc_note` rows. To keep both order-independent: Task 3's test should seed its OWN throwaway parent+notes inside the test body (insert a new `anc_post` 'delta' + 3 notes, soft-delete one, assert delta == 2) rather than mutating alpha's seed. Update Task 3's test accordingly — alpha's seed stays pristine (n1/n2 visible, n3 hidden) for this filter test. This is the cleaner harness; prefer it.
- [ ] **Run, expect FAIL** (method doesn't exist → compile error):
  ```
  cd crates && cargo test -p umbra-core --test annotate_count annotate_count_where_filters_children
  ```
- [ ] **Implement.** In `crates/umbra-core/src/orm/queryset/mod.rs`, add after `annotate_count` (~2130). The child predicate is `Predicate<C>`; render it to a backend-default `SimpleExpr` via the existing `cond_for` (use the default/postgres rendering — the count subquery embeds one expression; SQLite-specific predicate overrides aren't needed for the moderation-equality case, and `cond_for("postgres")` returns the default `cond`):
  ```rust
  /// Like [`Self::annotate_count`] but counts only the children
  /// matching `pred` — Django's `Count("comments", filter=Q(...))`.
  /// `C` is the CHILD model, so the predicate is typed against the
  /// child's columns (`comment::MODERATION.eq("visible")`). The
  /// predicate renders into the correlated count subquery's WHERE
  /// alongside the FK correlation and the auto soft-delete filter.
  ///
  /// ```rust,ignore
  /// Plugin::objects()
  ///     .annotate_count_where::<PluginComment>(
  ///         "visible_comments",
  ///         "comment_set",
  ///         plugin_comment::MODERATION.eq("visible"),
  ///     )
  /// ```
  pub fn annotate_count_where<C: crate::orm::Model>(
      mut self,
      alias: &str,
      relation: &str,
      pred: crate::orm::Predicate<C>,
  ) -> Self {
      // Render the child predicate to a backend-default SimpleExpr.
      // The count subquery embeds one expression; the equality /
      // comparison predicates used for child filters render the same
      // on both backends, so the default `cond` is correct.
      let child_filter = pred.cond_for("postgres");
      // Resolve the relation the same way annotate_related does, then
      // attach the child filter to the recorded annotation.
      let queryset = self.annotate_related(alias, relation, crate::orm::Aggregate::count());
      // The just-pushed annotation is the last one; attach the filter.
      let mut queryset = queryset;
      if let Some(last) = queryset.annotations.last_mut() {
          last.child_filter = Some(child_filter);
      }
      queryset
  }
  ```
  Note: `Predicate::cond_for` is `pub(crate)` and `annotate_count_where` lives in the same crate, so the call compiles. `mut self` is consumed into `queryset`; drop the leading `mut self` shadowing — simplify to take `self`, call `let mut queryset = self.annotate_related(...)`, set the filter, return. Final clean form:
  ```rust
  pub fn annotate_count_where<C: crate::orm::Model>(
      self,
      alias: &str,
      relation: &str,
      pred: crate::orm::Predicate<C>,
  ) -> Self {
      let child_filter = pred.cond_for("postgres");
      let mut queryset = self.annotate_related(alias, relation, crate::orm::Aggregate::count());
      if let Some(last) = queryset.annotations.last_mut() {
          last.child_filter = Some(child_filter);
      }
      queryset
  }
  ```
  (Confirm `crate::orm::Predicate` and `crate::orm::Model` are reachable from this module; `annotate_related` already references `crate::orm::Aggregate`, so the `crate::orm::` path is valid here.)
- [ ] **Run, expect PASS:**
  ```
  cd crates && cargo test -p umbra-core --test annotate_count annotate_count_where_filters_children
  cd crates && cargo test -p umbra-core --test annotate_count
  ```
- [ ] **Commit:**
  ```
  cd crates && cargo fmt && cargo clippy --all-targets && cargo build && cargo test
  ```
  `feat(orm): annotate_count_where renders a child predicate into the count`

---

## Task 5 — `annotate_count` over an M2M junction

**Files:**
- `crates/umbra-core/src/orm/queryset/mod.rs` (`annotate_related` resolution ~2088 — M2M fallback; subquery emission already handles `m2m_junction` from Task 3)
- Test: `crates/umbra-core/tests/annotate_count.rs` (attach 2 of 3 tags via real junction rows; count == 2).

- [ ] **Failing test.** Add an M2M relation to the harness. Add a `Tag` model and an M2M field on `Post`:
  ```rust
  #[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
  #[umbra(table = "anc_tag")]
  pub struct Tag {
      pub id: i64,
      pub name: String,
  }
  ```
  add to `Post` (the junction table is `anc_post_tags`, columns `parent_id` / `child_id`):
  ```rust
  #[umbra(m2m = "anc_tag")]
  pub tags: umbra::orm::M2M<Tag>,
  ```
  (the field needs `use umbra::orm::M2M;` or fully-qualified; `M2M` is not `#[sqlx(skip)]` here — follow the `filter_m2m.rs` harness which declares `pub tags: umbra::orm::M2M<Tag>` with `#[umbra(m2m = "fm2m_tag")]` and no sqlx skip). In `boot()` register `.model::<Tag>()`, create the tables, and seed:
  ```rust
  sqlx::query(
      "CREATE TABLE anc_tag (
          id INTEGER PRIMARY KEY AUTOINCREMENT,
          name TEXT NOT NULL
      )",
  )
  .execute(&pool).await.expect("CREATE TABLE anc_tag");
  sqlx::query(
      "CREATE TABLE anc_post_tags (
          parent_id INTEGER NOT NULL REFERENCES anc_post(id),
          child_id INTEGER NOT NULL REFERENCES anc_tag(id)
      )",
  )
  .execute(&pool).await.expect("CREATE TABLE anc_post_tags");
  for name in ["rust", "web", "orm"] {
      sqlx::query("INSERT INTO anc_tag (name) VALUES (?)")
          .bind(name).execute(&pool).await.expect("seed tag");
  }
  // alpha (post 1) gets 2 of the 3 tags via real junction rows.
  for (parent, child) in [(1, 1), (1, 2)] {
      sqlx::query("INSERT INTO anc_post_tags (parent_id, child_id) VALUES (?, ?)")
          .bind(parent).bind(child).execute(&pool).await.expect("seed junction");
  }
  ```
  Behavioral test:
  ```rust
  #[tokio::test]
  async fn annotate_count_over_m2m_counts_junction_rows() {
      boot().await;
      let rows = Post::objects()
          .annotate_count("tags")
          .fetch_annotated()
          .await
          .expect("fetch_annotated over m2m");
      let by_title: std::collections::HashMap<String, i64> = rows
          .into_iter()
          .map(|(p, a)| (p.title, a["tags_count"].as_i64().unwrap()))
          .collect();
      assert_eq!(by_title["alpha"], 2, "two junction rows attach to alpha");
      assert_eq!(by_title["beta"], 0, "beta has no tags");
  }
  ```
- [ ] **Run, expect FAIL** (`tags` isn't a reverse-FK relation → `annotate_count` poisons the annotation and `fetch_annotated` errors loudly):
  ```
  cd crates && cargo test -p umbra-core --test annotate_count annotate_count_over_m2m_counts_junction_rows
  ```
- [ ] **Implement — M2M fallback in resolution.** In `annotate_related` (`crates/umbra-core/src/orm/queryset/mod.rs` ~2088), when the name isn't in `REVERSE_FK_RELATIONS`, fall back to `T::M2M_RELATIONS` before producing the loud error. Restructure the resolution (building on Task 3's version):
  ```rust
  let rev_spec = T::REVERSE_FK_RELATIONS
      .iter()
      .find(|r| r.field_name == relation);
  let m2m_spec = T::M2M_RELATIONS
      .iter()
      .find(|r| r.field_name == relation);

  let pk = T::FIELDS
      .iter()
      .find(|f| f.primary_key)
      .map(|f| f.name)
      .unwrap_or("id");

  let mut child_soft_delete = false;
  let mut m2m_junction: Option<String> = None;

  let resolved = if let Some(spec) = rev_spec {
      child_soft_delete = spec.soft_delete;
      Ok((
          spec.target_table.to_string(),
          spec.fk_column.to_string(),
          T::TABLE.to_string(),
          pk.to_string(),
      ))
  } else if let Some(spec) = m2m_spec {
      // M2M count: junction table = "<parent>_<field>", columns
      // parent_id / child_id. The subquery counts junction rows.
      m2m_junction = Some(format!("{}_{}", T::TABLE, spec.field_name));
      // child_table / fk_column are unused for the M2M shape, but the
      // tuple still carries parent_table + parent_pk for correlation.
      Ok((
          spec.target_table.to_string(),
          "child_id".to_string(),
          T::TABLE.to_string(),
          pk.to_string(),
      ))
  } else {
      Err(format!(
          "umbra::orm::annotate_related: `{relation}` is not a reverse-FK or M2M relation on `{}` — reverse-FK relations: [{}], M2M relations: [{}]",
          T::NAME,
          T::REVERSE_FK_RELATIONS
              .iter()
              .map(|r| r.field_name)
              .collect::<Vec<_>>()
              .join(", "),
          T::M2M_RELATIONS
              .iter()
              .map(|r| r.field_name)
              .collect::<Vec<_>>()
              .join(", "),
      ))
  };

  self.annotations.push(RelatedAnnotation {
      alias: alias.to_string(),
      agg,
      resolved,
      child_soft_delete,
      child_filter: None,
      m2m_junction,
  });
  self
  ```
  The `build_query_for` M2M branch from Task 3 already emits `SELECT COUNT(*) FROM <junction> WHERE parent_id = <parent>.<pk>` when `m2m_junction` is `Some`. No further emission change needed.
- [ ] **Update the existing loud-failure test.** `unknown_relation_fails_loudly_everywhere` (~222) asserts the error mentions `comment_set`. The new message names both relation lists; confirm the assert `msg.contains("nope_set") && msg.contains("comment_set")` still holds (it does — `comment_set` is in the reverse-FK list). No change needed unless the assertion narrows; leave it.
- [ ] **Run, expect PASS:**
  ```
  cd crates && cargo test -p umbra-core --test annotate_count annotate_count_over_m2m_counts_junction_rows
  cd crates && cargo test -p umbra-core --test annotate_count
  ```
- [ ] **Commit:**
  ```
  cd crates && cargo fmt && cargo clippy --all-targets && cargo build && cargo test
  ```
  `feat(orm): annotate_count over M2M counts junction rows`

---

## Task 6 — Update the umbra.dev homepage to count visible-only + docs

**Files:**
- `umbra_website/plugins/public/src/lib.rs` (~62-88)
- `documentation/docs/v0.0.1/orm/aggregates.mdx`
- Verify: build the website workspace (standalone Cargo project under `umbra_website/`).

The homepage counts ALL comments including hidden/trashed. `PluginComment` has `moderation: CommentModeration` (the `Visible` variant string is `"visible"`) and `deleted_at` (soft-delete). Switch the count to visible-only; the soft-delete exclusion now rides automatically because `PluginComment` is `#[umbra(soft_delete)]` and `comment_set`'s spec carries `soft_delete: true` (Task 2). The column module for `PluginComment` is `plugin_comment` — confirm the generated module name and the `MODERATION` constant (the model field is `moderation`, so the constant is `plugin_comment::MODERATION`).

- [ ] **No new automated test** (the live-data homepage isn't unit-tested; the ORM behavior is pinned by Tasks 3-5). Verification is a build + the existing site behavior. Implement directly.
- [ ] **Implement.** In `umbra_website/plugins/public/src/lib.rs`, import the child column module and the comment model. Add to the `use plugin_directory::models::{...}` line: `plugin_comment` (the generated column module) and ensure `pd::PluginComment` is reachable (it is, via `pd::`). Change both `.annotate_count("comment_set")` sites:
  ```rust
  // line ~65 (the fetched query):
  .annotate_count_where::<pd::PluginComment>(
      "comment_set_count",
      "comment_set",
      pd::plugin_comment::MODERATION.eq("visible"),
  )
  ```
  Keep the alias `"comment_set_count"` so the downstream `anns.get("comment_set_count")` (line ~74) is unchanged. Do the same for the `explanation`/`to_sql` debug query at ~85 (or simplify — that block is a `println!` debug aid; update it to match or delete the debug `println!`). The `unwrap_or(0)` at ~76 stays.
  > If the generated column module isn't `plugin_comment`, resolve the real name by checking the derive output (the module is named after the snake_case struct name). The field constant is the upper-snake field name: `MODERATION`. The variant string for `CommentModeration::Visible` must match the DB literal — confirm `CommentModeration`'s `#[umbra(choices)]` lowercases to `"visible"` (it does, per the `kind`/`moderation` default `"pending"` convention in models.rs). Use the exact stored string.
- [ ] **Verify build** (website is a separate Cargo project — per CLAUDE.md, do NOT touch the dev server; just build):
  ```
  cd umbra_website && cargo build
  ```
  (Do NOT `cargo run`/restart the running dev server — the user runs it in autorefresh. A `cargo build` is safe and confirms the new ORM surface resolves against the website's path-dep on the framework.)
- [ ] **Docs.** Extend `documentation/docs/v0.0.1/orm/aggregates.mdx` with a short section: `annotate_count_where` (one example: `.annotate_count_where::<Comment>("visible", "comment_set", comment::MODERATION.eq("visible"))`), the automatic soft-delete exclusion (a soft-delete child is silently dropped from the count), and `annotate_count` over M2M (counts junction rows). One paragraph + one fenced code block each; link to `arch.md` / this spec for rationale per CLAUDE.md docs rule.
- [ ] **Commit** (framework + docs + website are one logical change — but the website is a separate Cargo project; commit the framework/docs change and the website change together as one feature commit since reverting them as a unit is the sensible undo):
  ```
  cd crates && cargo fmt && cargo clippy --all-targets && cargo build && cargo test
  cd umbra_website && cargo build
  ```
  `feat(orm): count visible-only comments on umbra.dev via annotate_count_where`

---

## Verification against spec Part 5

- **5a** (`soft_delete` onto `ReverseFkRelationSpec` + `ModelMeta`, derive-filled) → Tasks 1 + 2.
- **5b** (fold `deleted_at IS NULL` into the count for soft-delete children) → Task 3.
- **5c** (`annotate_count_where::<C>(alias, relation, Predicate<C>)`) → Task 4.
- **5d** (`annotate_count` resolves `M2M<T>` via `M2M_RELATIONS`, counts junction rows) → Task 5.
- **Homepage** (count visible-only on umbra.dev) → Task 6.

## Testing bar — behavioral, not tautological

- **Soft-delete:** Task 3 seeds 3 children, soft-deletes 1 via the *real* `delete()` soft-delete path, asserts count == 2; a zero-child parent asserts == 0 and is still returned (not dropped).
- **Filtered:** Task 4 mixes visible/hidden children, asserts the visible count via `annotate_count_where`.
- **M2M:** Task 5 attaches 2 of 3 candidates via real junction rows, asserts `annotate_count("tags")` == 2 (junction-row count); a no-tag parent asserts 0.

All extend the existing `crates/umbra-core/tests/annotate_count.rs` harness (real in-memory SQLite pool, real tables, real rows). Shared `OnceCell` state across tests is handled by seeding throwaway parents inside the mutating test (Task 3 note), keeping tests order-independent.
