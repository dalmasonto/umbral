# Query-Count Proofs — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prove every relation-loading path in the ORM is no-N+1 by *counting executed SQL statements* and showing the count is **constant as row counts grow from 10 to 10,000** — the artifact that survives a hostile "this framework is a sham" review.

**Architecture:** Extends the committed query-count harness (`crates/umbral-core/tests/query_counts.rs`, commit `a1f0ed4`) — a dedicated test binary with a tracing-layer statement counter, PRAGMA-filtered, serialized by a tokio mutex. Each proof seeds two dataset sizes, runs the same ORM operation against each, and asserts the operation issued the **same, small, fixed** number of statements regardless of size. A test that fails here means the feature has a real N+1 — fix the feature, not the test.

**Tech Stack:** Rust, sqlx, tracing-subscriber, the umbral ORM (`select_related` / `prefetch_related` / `join_related` / `annotate_count`).

**Depends on:** Plans A (M2M form validation batching), C (nested `join_related`), D (`annotate_count_where` + M2M counts). Run **after** those land — this plan tests their query behavior. The harness self-tests (`reading_many_rows_is_one_query_not_n`, etc.) already ship and pass.

---

## File Structure

- **Modify** `crates/umbral-core/tests/query_counts.rs` — add the shared test-model boot block + five scale-proof tests to the existing harness binary. Keeping them in the *same* binary is deliberate: the counter is process-global, so the proofs must share the one process the harness owns. Every new test takes `query_lock()` first.

All tests follow the harness contract (from its module docs): take the lock, seed under the lock, `reset()` immediately before the measured op, assert on `count()` with `statements()` in the failure message.

---

## Shared setup (added once, used by every task)

- [ ] **Step 1: Add the test models + boot helper to `query_counts.rs`**

Append below the existing self-tests. Mirrors the `App::builder()` + raw-DDL boot pattern from `crates/umbral-core/tests/annotate_count.rs`, but with a parent→FK→FK chain (for joins/select_related), a reverse set (for prefetch + annotate), and an M2M (for M2M proofs).

```rust
use tokio::sync::OnceCell as TokioOnceCell;
use umbral::orm::{ForeignKey, M2M, ReverseSet};
use umbral::prelude::*;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]
pub struct Author {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]
pub struct Plugin {
    pub id: i64,
    pub name: String,
    pub author: ForeignKey<Author>, // NOT NULL → INNER under auto-inference
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]
pub struct Tag {
    pub id: i64,
    pub label: String,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]
pub struct Comment {
    pub id: i64,
    pub body: String,
    pub plugin: ForeignKey<Plugin>,
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbral(reverse_fk = "comment")]
    pub reaction_set: ReverseSet<Reaction>,
    #[sqlx(skip)]
    #[serde(skip)]
    pub tags: M2M<Tag>,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]
pub struct Reaction {
    pub id: i64,
    pub kind: String,
    pub comment: ForeignKey<Comment>,
}

static BOOT: TokioOnceCell<()> = TokioOnceCell::const_new();

/// Boot the App once with the proof models against an in-memory SQLite
/// pool, create the tables, and seed `n` Comments each pointing at a
/// Plugin → Author chain, each with one Reaction and two Tags. Idempotent
/// per process; re-seeds up to `n` rows so a later larger-N call tops up.
async fn boot_and_seed(n: i64) {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let pool = umbral::db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Author>()
            .model::<Plugin>()
            .model::<Tag>()
            .model::<Comment>()
            .model::<Reaction>()
            .build()
            .expect("App::build");
        for ddl in [
            "CREATE TABLE author (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
            "CREATE TABLE plugin (id INTEGER PRIMARY KEY, name TEXT NOT NULL, author INTEGER NOT NULL)",
            "CREATE TABLE tag (id INTEGER PRIMARY KEY, label TEXT NOT NULL)",
            "CREATE TABLE comment (id INTEGER PRIMARY KEY, body TEXT NOT NULL, plugin INTEGER NOT NULL)",
            "CREATE TABLE reaction (id INTEGER PRIMARY KEY, kind TEXT NOT NULL, comment INTEGER NOT NULL)",
            "CREATE TABLE comment_tags (parent_id INTEGER NOT NULL, child_id INTEGER NOT NULL)",
            "INSERT INTO author (id, name) VALUES (1, 'Ada')",
            "INSERT INTO plugin (id, name, author) VALUES (1, 'orm', 1)",
            "INSERT INTO tag (id, label) VALUES (1, 'perf'), (2, 'safety')",
        ] {
            sqlx::query(ddl).execute(&pool).await.unwrap();
        }
    })
    .await;

    // Top up to `n` comments (+ one reaction + two tag links each).
    let pool = umbral::db::pool_for("default").expect("booted pool");
    let have: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM comment")
        .fetch_one(&pool)
        .await
        .unwrap();
    for id in (have + 1)..=n {
        sqlx::query("INSERT INTO comment (id, body, plugin) VALUES (?, ?, 1)")
            .bind(id)
            .bind(format!("c{id}"))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO reaction (kind, comment) VALUES ('up', ?)")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO comment_tags (parent_id, child_id) VALUES (?, 1), (?, 2)")
            .bind(id)
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
    }
}
```

- [ ] **Step 2: Confirm `umbral::db::pool_for` (or the equivalent ambient-pool accessor) exists**

Run: `cd crates && grep -rn "pub fn pool_for\|pub fn pool(" umbral-core/src/db.rs`
Expected: a public accessor returning the named pool. If the name differs (e.g. `pool()` for the default), use that exact name in the helper above and in every task. Do NOT invent one.

- [ ] **Step 3: Verify the harness still builds with the new models**

Run: `cd crates && cargo test -p umbral-core --test query_counts -- --list`
Expected: lists the two existing self-tests; compiles clean. (No new tests yet.)

- [ ] **Step 4: Commit the shared setup**

```bash
cd crates && cargo fmt && cargo clippy --all-targets && cargo build && cargo test -p umbral-core --test query_counts
git add crates/umbral-core/tests/query_counts.rs
git commit -m "test(orm): proof models + boot/seed helper for query-count scale tests"
```

---

## Task 1: `select_related` is `1 + hops` queries, invariant to parent count

The nested batched-IN path. Two hops (`plugin__author`) must be exactly 3 statements (main + 1 per hop) whether 10 or 10,000 parents — never `1 + N`.

**Files:** Modify `crates/umbral-core/tests/query_counts.rs` (Test).

- [ ] **Step 1: Write the proof**

```rust
#[tokio::test]
async fn select_related_nested_is_constant_queries_not_n_plus_1() {
    let _g = query_lock().await;
    let mut counts = Vec::new();
    for n in [10_i64, 10_000] {
        boot_and_seed(n).await;
        reset();
        let rows = Comment::objects()
            .select_related("plugin__author")
            .fetch()
            .await
            .expect("fetch");
        assert_eq!(rows.len() as i64, n, "sanity: all parents returned");
        // Deepest level hydrated from the batched chain, not per-row.
        let author = rows[0]
            .plugin
            .resolved()
            .and_then(|p| p.author.resolved())
            .expect("author hydrated");
        assert_eq!(author.name, "Ada");
        counts.push(count());
    }
    // 1 main + 2 hop batches = 3, for BOTH sizes. Equal counts is the
    // no-N+1 proof; the absolute value (3) is the select_related contract.
    assert_eq!(
        counts[0], counts[1],
        "query count must not grow with parent count (saw {counts:?}; statements: {:?})",
        statements()
    );
    assert_eq!(counts[0], 3, "expected main + 2 hop batches; saw {:?}", statements());
}
```

- [ ] **Step 2: Run — expect PASS (or a real N+1 bug surfaced)**

Run: `cd crates && cargo test -p umbral-core --test query_counts select_related_nested_is_constant -- --nocapture`
Expected: PASS. If `counts == [3, 3]` it proves O(1)-in-rows. If the large-N count is bigger (e.g. `[3, 10001]`), `select_related` has an N+1 — open systematic-debugging on `hydration.rs` rather than weakening the assertion.

- [ ] **Step 3: Commit**

```bash
cd crates && cargo fmt && cargo clippy --all-targets && cargo test -p umbral-core --test query_counts
git add crates/umbral-core/tests/query_counts.rs
git commit -m "test(orm): prove select_related is 1+hops queries, invariant to row count"
```

---

## Task 2: `prefetch_related` is `1 + 1` per relation, invariant to parent count

Reverse-FK (`reaction_set`) and M2M (`tags`) collections must each cost one extra batched query, never one-per-parent.

**Files:** Modify `crates/umbral-core/tests/query_counts.rs` (Test).

- [ ] **Step 1: Write the proof**

```rust
#[tokio::test]
async fn prefetch_related_is_constant_queries_not_n_plus_1() {
    let _g = query_lock().await;
    let mut counts = Vec::new();
    for n in [10_i64, 10_000] {
        boot_and_seed(n).await;
        reset();
        let rows = Comment::objects()
            .prefetch_related("reaction_set")
            .prefetch_related("tags")
            .fetch()
            .await
            .expect("fetch");
        assert_eq!(rows.len() as i64, n);
        assert_eq!(rows[0].reaction_set.resolved().map(|r| r.len()), Some(1));
        assert_eq!(rows[0].tags.resolved().map(|t| t.len()), Some(2));
        counts.push(count());
    }
    assert_eq!(
        counts[0], counts[1],
        "prefetch query count must not grow with parent count (saw {counts:?}; {:?})",
        statements()
    );
    // 1 main + 1 reverse-fk batch + 1 m2m batch = 3.
    assert_eq!(counts[0], 3, "expected main + 2 prefetch batches; saw {:?}", statements());
}
```

- [ ] **Step 2: Run — expect PASS**

Run: `cd crates && cargo test -p umbral-core --test query_counts prefetch_related_is_constant -- --nocapture`
Expected: `counts == [3, 3]`. A growing large-N count means the reverse-FK or M2M hydration loops per parent — fix `hydration.rs`.

- [ ] **Step 3: Commit**

```bash
cd crates && cargo fmt && cargo clippy --all-targets && cargo test -p umbral-core --test query_counts
git add crates/umbral-core/tests/query_counts.rs
git commit -m "test(orm): prove prefetch_related is 1+1 per relation, invariant to row count"
```

---

## Task 3: nested `join_related` is exactly 1 query, invariant to parent count (Plan C)

**Files:** Modify `crates/umbral-core/tests/query_counts.rs` (Test).

- [ ] **Step 1: Write the proof**

```rust
#[tokio::test]
async fn nested_join_related_is_one_query_not_n() {
    let _g = query_lock().await;
    let mut counts = Vec::new();
    for n in [10_i64, 10_000] {
        boot_and_seed(n).await;
        reset();
        let rows = Comment::objects()
            .inner_join_related("plugin__author")
            .fetch()
            .await
            .expect("fetch");
        assert_eq!(rows.len() as i64, n);
        let author = rows[0]
            .plugin
            .resolved()
            .and_then(|p| p.author.resolved())
            .expect("author hydrated from the single joined query");
        assert_eq!(author.name, "Ada");
        counts.push(count());
    }
    assert_eq!(
        counts[0], counts[1],
        "join query count must not grow with parent count (saw {counts:?}; {:?})",
        statements()
    );
    assert_eq!(counts[0], 1, "a nested join is ONE statement; saw {:?}", statements());
}
```

- [ ] **Step 2: Run — expect PASS**

Run: `cd crates && cargo test -p umbral-core --test query_counts nested_join_related_is_one -- --nocapture`
Expected: `counts == [1, 1]`. This is the strongest deep-join claim: the whole `comment → plugin → author` graph in one statement, flat across 10 → 10,000 rows.

- [ ] **Step 3: Commit**

```bash
cd crates && cargo fmt && cargo clippy --all-targets && cargo test -p umbral-core --test query_counts
git add crates/umbral-core/tests/query_counts.rs
git commit -m "test(orm): prove nested join_related is one query, invariant to row count"
```

---

## Task 4: `annotate_count` (+ `_where`, + M2M) is 1 query, invariant to parent count (Plan D)

**Files:** Modify `crates/umbral-core/tests/query_counts.rs` (Test).

- [ ] **Step 1: Write the proof**

```rust
#[tokio::test]
async fn annotate_count_is_one_query_not_n() {
    let _g = query_lock().await;
    let mut counts = Vec::new();
    for n in [10_i64, 10_000] {
        boot_and_seed(n).await;
        reset();
        // Correlated subquery rides in the main SELECT — one statement for
        // all N parents. Both the reverse-FK count and the M2M count.
        let rows = Comment::objects()
            .annotate_count("reaction_set")
            .annotate_count("tags")
            .fetch_annotated()
            .await
            .expect("fetch_annotated");
        assert_eq!(rows.len() as i64, n);
        assert_eq!(rows[0].1["reaction_set_count"].as_i64(), Some(1));
        assert_eq!(rows[0].1["tags_count"].as_i64(), Some(2));
        counts.push(count());
    }
    assert_eq!(
        counts[0], counts[1],
        "annotate query count must not grow with parent count (saw {counts:?}; {:?})",
        statements()
    );
    assert_eq!(counts[0], 1, "annotate is one correlated query; saw {:?}", statements());
}
```

- [ ] **Step 2: Run — expect PASS**

Run: `cd crates && cargo test -p umbral-core --test query_counts annotate_count_is_one -- --nocapture`
Expected: `counts == [1, 1]`. Two annotations still ride in the single SELECT.

- [ ] **Step 3: Commit**

```bash
cd crates && cargo fmt && cargo clippy --all-targets && cargo test -p umbral-core --test query_counts
git add crates/umbral-core/tests/query_counts.rs
git commit -m "test(orm): prove annotate_count is one query, invariant to row count"
```

---

## Task 5: M2M form validation is 1 query, invariant to selected-id count (Plan A)

Proves Plan A's batching fix: validating M submitted M2M ids is one `IN` query, not M `COUNT`s.

**Files:** Modify `crates/umbral-core/tests/query_counts.rs` (Test).

- [ ] **Step 1: Seed a wide tag set, then write the proof**

Add a small helper that tops the `tag` table up to `k` rows, then:

```rust
#[tokio::test]
async fn m2m_validation_is_one_query_not_per_id() {
    let _g = query_lock().await;
    boot_and_seed(1).await;
    let pool = umbral::db::pool_for("default").expect("pool");
    // Ensure at least 500 candidate tags exist.
    let have: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tag").fetch_one(&pool).await.unwrap();
    for id in (have + 1)..=500 {
        sqlx::query("INSERT INTO tag (id, label) VALUES (?, ?)")
            .bind(id).bind(format!("t{id}")).execute(&pool).await.unwrap();
    }

    let mut counts = Vec::new();
    for k in [3_usize, 300] {
        let ids: Vec<String> = (1..=k as i64).map(|i| i.to_string()).collect();
        let mut errs = umbral::forms::ValidationErrors::new();
        reset();
        // The batched validator from Plan A (Task 7). Name/path per Plan A.
        let _ok = umbral::forms::validate_multi_fk_exists("tags", &ids, "tag", &mut errs).await;
        assert!(errs.is_empty(), "all ids exist");
        counts.push(count());
    }
    assert_eq!(
        counts[0], counts[1],
        "M2M validation must be one query regardless of how many ids (saw {counts:?}; {:?})",
        statements()
    );
    assert_eq!(counts[0], 1, "validating M ids is ONE IN-query; saw {:?}", statements());
}
```

- [ ] **Step 2: Run — expect PASS**

Run: `cd crates && cargo test -p umbral-core --test query_counts m2m_validation_is_one -- --nocapture`
Expected: `counts == [1, 1]`. If the 300-id case shows ~300, Plan A's batching regressed to a per-id loop — fix `validate_multi_fk_exists`, not this test. (Confirm the public path/name of `validate_multi_fk_exists` against Plan A; if it lives behind a different module, call it there.)

- [ ] **Step 3: Commit**

```bash
cd crates && cargo fmt && cargo clippy --all-targets && cargo test -p umbral-core --test query_counts
git add crates/umbral-core/tests/query_counts.rs
git commit -m "test(orm): prove M2M form validation is one query, invariant to id count"
```

---

## Self-review checklist

- Every proof loops over a **small and a large** N and asserts the counts are **equal** (the data-size-invariance proof) AND match the expected absolute value (the per-feature contract). Equal-but-wrong-absolute still catches a constant-factor regression; unequal catches an N+1.
- Failure messages always print `statements()` so a hostile reviewer (or future you) sees exactly what ran.
- These tests are *verification*, not feature work: a failure means the feature under test has an N+1 — the fix lands in the feature crate, and the assertion is never weakened to make red go green.
- Absolute expected values to confirm against the landed implementations: select_related nested 2-hop = 3; prefetch (reverse-fk + m2m) = 3; nested join = 1; annotate ×2 = 1; M2M validate = 1. If an implementation legitimately differs (e.g. select_related batches hops differently), update the absolute assertion to the real constant **and** keep the equal-across-N assertion — that one never changes.
```
