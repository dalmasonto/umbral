//! `QuerySet::join_related` — true LEFT JOIN variant of FK prefetch.
//!
//! Counterpart to `select_related` which runs a batched second
//! query. `join_related` weaves a `LEFT JOIN <related> ON ...` into
//! the main SELECT (with aliased child columns `<field>__<col>`) so
//! one round-trip pulls parent + related rows together.
//!
//! Tests cover: SQL emits the JOIN, hydrated parent's
//! `ForeignKey::resolved` matches the joined child, LEFT JOIN miss
//! (nullable FK pointing nowhere) leaves the FK unresolved, the
//! Manager forwarder works, and that `.only()` composes (the typed
//! terminal still errors loudly when combined).

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbral::orm::ForeignKey;
use umbral_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "jr_category")]
pub struct Category {
    pub id: i64,
    #[umbral(string)]
    pub name: String,
    pub slug: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "jr_product")]
pub struct Product {
    pub id: i64,
    #[umbral(string)]
    pub name: String,
    pub category: ForeignKey<Category>,
    pub brand: Option<ForeignKey<Category>>,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let pool = db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Category>()
            .model::<Product>()
            .build()
            .expect("App::build");

        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
        for (name, slug) in &[("Coffee", "coffee"), ("Tea", "tea")] {
            sqlx::query("INSERT INTO jr_category (name, slug) VALUES (?, ?)")
                .bind(*name)
                .bind(*slug)
                .execute(&pool)
                .await
                .expect("seed category");
        }
        // Three products:
        //   alpha → category=Coffee(1), brand=Tea(2)
        //   beta  → category=Tea(2),    brand=NULL  (LEFT JOIN miss on brand)
        //   gamma → category=Coffee(1), brand=Coffee(1)
        for (name, cat, brand) in &[
            ("alpha", 1_i64, Some(2_i64)),
            ("beta", 2, None),
            ("gamma", 1, Some(1)),
        ] {
            sqlx::query("INSERT INTO jr_product (name, category, brand) VALUES (?, ?, ?)")
                .bind(*name)
                .bind(*cat)
                .bind(*brand)
                .execute(&pool)
                .await
                .expect("seed product");
        }
    })
    .await;
}

#[tokio::test]
async fn to_sql_emits_inner_join_with_aliased_child_columns() {
    boot().await;
    let sql = Product::objects()
        .filter(product::ID.eq(1))
        .join_related("category")
        .to_sql();
    // The JOIN itself must be present. `Product.category` is a NOT NULL
    // FK, so plain `join_related` now auto-infers INNER (gap 4c):
    // a NOT NULL FK can't be a LEFT-JOIN miss, so INNER
    // is correct and lets the planner drop the outer-join bookkeeping).
    // The nullable `brand` FK still LEFT-joins — see
    // `left_join_miss_for_null_fk_leaves_field_as_none`.
    assert!(sql.contains("INNER JOIN"), "expected INNER JOIN in: {sql}");
    assert!(
        sql.contains("\"jr_category\""),
        "expected related table in JOIN: {sql}"
    );
    // Aliased child columns: <field>__<col>. The category model has
    // id / name / slug — all three should show up as aliased cols.
    assert!(
        sql.contains("\"category__name\""),
        "expected aliased child col: {sql}"
    );
    assert!(
        sql.contains("\"category__slug\""),
        "expected aliased child col: {sql}"
    );
    assert!(
        sql.contains("\"category__id\""),
        "expected aliased child col: {sql}"
    );
    // Parent columns still ride along — unaliased.
    assert!(
        sql.contains("\"jr_product\".\"name\"") || sql.contains("\"id\""),
        "expected parent cols too: {sql}"
    );
}

#[tokio::test]
async fn join_related_hydrates_foreign_key_resolved_slot() {
    boot().await;
    let products = Product::objects()
        .filter(product::NAME.eq("alpha"))
        .join_related("category")
        .fetch()
        .await
        .expect("fetch");
    assert_eq!(products.len(), 1);
    let p = &products[0];
    // alpha.category points at Coffee(id=1).
    let cat = p.category.resolved().expect("category hydrated");
    assert_eq!(cat.id, 1);
    assert_eq!(cat.name, "Coffee");
}

#[tokio::test]
async fn left_join_miss_for_null_fk_leaves_field_as_none() {
    boot().await;
    // For `pub brand: Option<ForeignKey<Category>>` with a NULL
    // brand column, the FromRow decode produces `None` for the
    // whole field — there's no FK wrapper to hold a "resolved"
    // slot. The join_related path must not error on the NULL row;
    // the LEFT JOIN miss leaves every aliased brand__col NULL, the
    // PK-is-null guard short-circuits hydration, and the parent
    // row decodes cleanly.
    let products = Product::objects()
        .filter(product::NAME.eq("beta"))
        .join_related("brand")
        .fetch()
        .await
        .expect("fetch must not error on a LEFT JOIN miss");
    assert_eq!(products.len(), 1);
    assert!(
        products[0].brand.is_none(),
        "brand column was NULL → field is None"
    );
}

#[tokio::test]
async fn join_related_many_hydrates_two_fks_in_one_query() {
    boot().await;
    let products = Product::objects()
        .filter(product::NAME.eq("gamma"))
        .join_related_many(&["category", "brand"])
        .fetch()
        .await
        .expect("fetch");
    assert_eq!(products.len(), 1);
    let p = &products[0];
    // gamma → category=Coffee(1), brand=Coffee(1) (same id, both FKs
    // join the same row of the related table — confirms each
    // aliased prefix decodes independently).
    assert_eq!(p.category.resolved().expect("cat").name, "Coffee");
    let brand_inner = p
        .brand
        .as_ref()
        .expect("brand wrapper")
        .resolved()
        .expect("brand resolved");
    assert_eq!(brand_inner.name, "Coffee");
}

#[tokio::test]
async fn manager_join_related_forwards_to_queryset() {
    boot().await;
    let sql = Product::objects().join_related("category").to_sql();
    // NOT NULL FK -> inferred INNER (gap 4c), same as
    // `to_sql_emits_inner_join_with_aliased_child_columns`.
    assert!(sql.contains("INNER JOIN"));
    assert!(sql.contains("\"category__name\""));
}

#[tokio::test]
async fn unknown_field_name_is_silently_skipped_in_sql() {
    boot().await;
    let sql = Product::objects().join_related("nope_not_a_field").to_sql();
    // Unknown name in to_sql() → no JOIN emitted (silent skip — the
    // SQL inspection surface is debug-only, so we render cleanly
    // instead of panicking). The fetch() path is loud — see
    // `unknown_field_name_fetch_errors_loudly` below.
    assert!(!sql.contains("LEFT JOIN"), "should be no JOIN: {sql}");
}

#[tokio::test]
async fn only_with_join_related_trims_inner_subquery_columns() {
    boot().await;
    // When .only() narrows the outer projection, the inner subquery
    // should only carry the columns the outer still consumes — the
    // parent columns named in .only() (intersected with T::FIELDS so
    // joined-child aliases don't leak) plus the FK column the JOIN
    // ON clause needs. Skipping per-row columns the outer drops
    // anyway is a measurable win on wide tables.
    let sql = Product::objects()
        .only(&["id", "name", "category__name"])
        .join_related("category")
        .filter(product::ID.eq(1))
        .to_sql();
    // Outer SELECT: only the three requested columns.
    assert!(
        sql.contains("SELECT \"id\", \"name\", \"category__name\""),
        "outer projection: {sql}"
    );
    // Inner subquery now carries ONLY: id, name, category (the FK
    // needed for the JOIN ON). brand should NOT appear — it's a
    // parent column the outer never touches.
    let inner_start = sql.find("FROM (SELECT").expect("subquery wrap: {sql}");
    let inner_end = inner_start
        + sql[inner_start..]
            .find(") AS \"__p\"")
            .expect("subquery close: {sql}");
    let inner = &sql[inner_start..inner_end];
    assert!(
        inner.contains("\"id\""),
        "inner needs id (in only): {inner}"
    );
    assert!(
        inner.contains("\"name\""),
        "inner needs name (in only): {inner}"
    );
    assert!(
        inner.contains("\"category\""),
        "inner needs category (FK for JOIN ON): {inner}"
    );
    assert!(
        !inner.contains("\"brand\""),
        "brand must be trimmed: {inner}"
    );
}

#[tokio::test]
async fn join_related_without_only_keeps_full_inner_select() {
    boot().await;
    // Without .only(), the inner subquery keeps every parent
    // column — the pre-#46 behaviour, byte-for-byte. We only trim
    // when the outer projection has been narrowed.
    let sql = Product::objects().join_related("category").to_sql();
    let inner_start = sql.find("FROM (SELECT").expect("subquery wrap: {sql}");
    let inner_end = inner_start
        + sql[inner_start..]
            .find(") AS \"__p\"")
            .expect("subquery close: {sql}");
    let inner = &sql[inner_start..inner_end];
    assert!(
        inner.contains("\"brand\""),
        "no .only() → full parent col list: {inner}"
    );
}

#[tokio::test]
async fn unknown_field_name_fetch_errors_loudly() {
    boot().await;
    let err = Product::objects()
        .join_related("nope_not_a_field")
        .fetch()
        .await
        .expect_err("fetch must reject unknown join field");
    let msg = err.to_string();
    assert!(
        msg.contains("nope_not_a_field"),
        "error names the bad field: {msg}"
    );
    assert!(
        msg.contains("join_related"),
        "error names the method: {msg}"
    );
}

#[tokio::test]
async fn join_related_composes_with_filter_and_order_by() {
    boot().await;
    // Should return all products with category=Coffee (id=1) in
    // ascending id order. alpha (id=1) and gamma (id=3).
    let products = Product::objects()
        .filter(product::CATEGORY.eq(1))
        .order_by(product::ID.asc())
        .join_related("category")
        .fetch()
        .await
        .expect("fetch");
    assert_eq!(products.len(), 2);
    assert_eq!(products[0].name, "alpha");
    assert_eq!(products[1].name, "gamma");
    // Both should have category hydrated.
    for p in &products {
        let cat = p.category.resolved().expect("hydrated");
        assert_eq!(cat.name, "Coffee");
    }
}
