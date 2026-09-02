//! gaps4 #76 — forward FK / O2O `__` traversal on the FILTER (WHERE) side.
//!
//! Phase 1 taught the read side (`select_related` / relation accessors) to walk
//! `__` FK hops; this closes the filter side so `Developer::objects().filter(
//! user__username == "x")` resolves through the FK in ONE query — the Django
//! "find a parent by a related field" move that previously needed two queries
//! (and, via `Predicate::col_eq("user__username", …)`, produced a literal-column
//! SQL error).
//!
//! Behavioral, per the project's testing rule: real rows through the actual
//! public filter path, read the object back, and a `to_sql` probe ALONGSIDE the
//! round-trip proving it is a single statement (a subquery, no second query).

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbral::orm::{ForeignKey, Predicate};
use umbral_core::db;

// =========================================================================
// Models —  Company <--FK-- User <--FK-- Developer, plus a nullable FK.
//   Developer --FK--> User        (required, "user")
//   Developer --Option<FK>--> User (nullable, "mentor")
//   User      --FK--> Company     (required, "company")
// A 3-level forward chain exercises single-hop and 2-hop traversal.
// =========================================================================

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "frt_company")]
pub struct Company {
    #[umbral(primary_key)]
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "frt_user")]
pub struct User {
    #[umbral(primary_key)]
    pub id: i64,
    pub username: String,
    pub company: ForeignKey<Company>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "frt_developer")]
pub struct Developer {
    #[umbral(primary_key)]
    pub id: i64,
    pub name: String,
    pub user: ForeignKey<User>,
    pub mentor: Option<ForeignKey<User>>,
}

// =========================================================================
// Harness
// =========================================================================

static BOOT: OnceCell<sqlx::SqlitePool> = OnceCell::const_new();

async fn boot() -> sqlx::SqlitePool {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let pool = db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Company>()
            .model::<User>()
            .model::<Developer>()
            .build()
            .expect("App::build");

        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        // Companies: acme=1, globex=2.
        for name in &["acme", "globex"] {
            sqlx::query("INSERT INTO frt_company (name) VALUES (?)")
                .bind(*name)
                .execute(&pool)
                .await
                .expect("seed company");
        }
        // Users: ada@acme=1, grace@globex=2, linus@acme=3.
        for (username, company) in &[("ada", 1_i64), ("grace", 2), ("linus", 1)] {
            sqlx::query("INSERT INTO frt_user (username, company) VALUES (?, ?)")
                .bind(*username)
                .bind(*company)
                .execute(&pool)
                .await
                .expect("seed user");
        }
        // Developers: dev-ada -> user 1 (mentor 3); dev-grace -> user 2 (no mentor).
        sqlx::query("INSERT INTO frt_developer (name, user, mentor) VALUES ('dev-ada', 1, 3)")
            .execute(&pool)
            .await
            .expect("seed dev 1");
        sqlx::query("INSERT INTO frt_developer (name, user, mentor) VALUES ('dev-grace', 2, NULL)")
            .execute(&pool)
            .await
            .expect("seed dev 2");

        pool
    })
    .await
    .clone()
}

// =========================================================================
// (1) String form — `user__username == "ada"` resolves via one subquery.
// =========================================================================

#[tokio::test]
async fn string_form_single_hop_resolves_in_one_query() {
    boot().await;

    let pred =
        Predicate::<Developer>::related("user__username", "ada").expect("valid relation path");

    // to_sql probe ALONGSIDE the round-trip: exactly ONE statement, and it is a
    // correlated IN-subquery over the related table (no JOIN, no second query).
    let sql = Developer::objects().filter(pred.clone()).to_sql();
    assert_eq!(
        sql.matches("SELECT").count(),
        2,
        "one outer SELECT + one subquery: {sql}"
    );
    assert!(
        sql.contains("IN (SELECT"),
        "resolves via an IN-subquery: {sql}"
    );
    assert!(
        sql.contains("frt_user"),
        "subquery targets the related table: {sql}"
    );
    assert!(
        !sql.contains("JOIN"),
        "forward FK filter uses a subquery, not a JOIN: {sql}"
    );

    let dev = Developer::objects()
        .filter(pred)
        .first()
        .await
        .expect("query runs")
        .expect("dev-ada matches");
    assert_eq!(dev.name, "dev-ada");
}

// =========================================================================
// (2) Typed form — same result, value-type-checked, no registry needed.
// =========================================================================

#[tokio::test]
async fn typed_form_single_hop_matches_string_form() {
    boot().await;

    let dev = Developer::objects()
        .filter(Developer::USER.to(User::USERNAME.eq("ada")))
        .first()
        .await
        .expect("query runs")
        .expect("dev-ada matches");
    assert_eq!(dev.name, "dev-ada");

    // grace's user is grace, not ada — excluded.
    let count = Developer::objects()
        .filter(Developer::USER.to(User::USERNAME.eq("grace")))
        .count()
        .await
        .expect("count runs");
    assert_eq!(count, 1, "only dev-grace matches user=grace");
}

// =========================================================================
// (3) Two-hop chain — `user__company__name` (string) and nested `.to` (typed).
// =========================================================================

#[tokio::test]
async fn two_hop_chain_string_and_typed() {
    boot().await;

    // String: developers whose user's company is "acme" → only dev-ada.
    let names: Vec<String> = Developer::objects()
        .filter(Predicate::<Developer>::related("user__company__name", "acme").expect("path"))
        .order_by(developer::NAME.asc())
        .fetch()
        .await
        .expect("fetch")
        .into_iter()
        .map(|d| d.name)
        .collect();
    assert_eq!(names, vec!["dev-ada"], "only dev-ada's user is at acme");

    // Two-hop probe: two nested subqueries (three SELECTs total).
    let sql = Developer::objects()
        .filter(Predicate::<Developer>::related("user__company__name", "acme").unwrap())
        .to_sql();
    assert_eq!(
        sql.matches("SELECT").count(),
        3,
        "outer + 2 nested subqueries: {sql}"
    );

    // Typed nested form produces the same row.
    let dev = Developer::objects()
        .filter(Developer::USER.to(User::COMPANY.to(Company::NAME.eq("acme"))))
        .first()
        .await
        .expect("query runs")
        .expect("dev-ada matches via nested .to");
    assert_eq!(dev.name, "dev-ada");
}

// =========================================================================
// (4) A non-existent `__` field errors clearly — never a literal-column 500.
// =========================================================================

#[tokio::test]
async fn bad_relation_path_errors_clearly() {
    boot().await;

    // Unknown leading field.
    let err = Predicate::<Developer>::related("nope__username", "x")
        .err()
        .expect("unknown field must error");
    let msg = err.to_string();
    assert!(msg.contains("nope"), "names the offending field: {msg}");

    // A non-FK leading field (a plain column can't be traversed through).
    let err = Predicate::<Developer>::related("name__username", "x")
        .err()
        .expect("non-FK hop must error");
    assert!(
        err.to_string().contains("not a forward relation"),
        "explains the non-FK hop: {err}"
    );

    // Unknown leaf column on the related table.
    let err = Predicate::<Developer>::related("user__nope", "x")
        .err()
        .expect("unknown leaf must error");
    assert!(
        err.to_string().contains("nope"),
        "names the missing leaf: {err}"
    );
}

// =========================================================================
// (5) Nullable FK traversal + a lookup suffix.
// =========================================================================

#[tokio::test]
async fn nullable_fk_and_lookup_suffix() {
    boot().await;

    // Nullable FK: dev-ada's mentor is linus; dev-grace has NULL mentor (excluded).
    let dev = Developer::objects()
        .filter(Predicate::<Developer>::related("mentor__username", "linus").expect("path"))
        .first()
        .await
        .expect("runs")
        .expect("dev-ada has mentor linus");
    assert_eq!(dev.name, "dev-ada");

    // Typed nullable form.
    let count = Developer::objects()
        .filter(Developer::MENTOR.to(User::USERNAME.eq("linus")))
        .count()
        .await
        .expect("count");
    assert_eq!(count, 1);

    // Lookup suffix: icontains on the related username.
    let count = Developer::objects()
        .filter(Predicate::<Developer>::related("user__username__icontains", "AD").expect("path"))
        .count()
        .await
        .expect("count");
    assert_eq!(count, 1, "case-insensitive substring 'ad' matches ada");
}
