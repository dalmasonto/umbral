//! gaps4 #94 — live-Postgres proof that a dynamic M2M write (admin form /
//! REST `PATCH`) whose child ids arrive as JSON **strings** binds the junction
//! `child_id`/`parent_id` as `bigint`, not `text`.
//!
//! The bug: the dynamic junction writer bound each id by its JSON runtime
//! shape, so `["1","2"]` became TEXT parameters. SQLite coerces `text`↔`bigint`
//! silently, hiding it; Postgres is strict and rejected the write with
//! `column "child_id" is of type bigint but expression is of type text`, so
//! every live M2M edit 500'd on Postgres. The SQLite round-trip can't catch
//! this — only a real Postgres write does.
//!
//! Gated on `UMBRAL_TEST_POSTGRES_URL`. Its own binary because `App::build`
//! publishes process-global state (one build per binary).

use serde::{Deserialize, Serialize};
use serde_json::json;
use umbral::orm::{DynQuerySet, M2M};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "m2mpg_tag")]
pub struct Tag {
    pub id: i64,
    #[umbral(string)]
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "m2mpg_post")]
pub struct Post {
    pub id: i64,
    #[umbral(string)]
    pub title: String,
    /// M2M to Tag — junction table `m2mpg_post_tags`.
    #[sqlx(skip)]
    #[umbral(m2m = "m2mpg_tag")]
    pub tags: M2M<Tag>,
}

#[tokio::test]
#[ignore = "needs UMBRAL_TEST_POSTGRES_URL pointing at a Postgres server"]
async fn dynamic_m2m_string_child_ids_bind_bigint_on_postgres() {
    let Ok(url) = std::env::var("UMBRAL_TEST_POSTGRES_URL") else {
        eprintln!("skipping: UMBRAL_TEST_POSTGRES_URL not set");
        return;
    };
    let db = umbral::db::connect(&url)
        .await
        .expect("connect to Postgres");
    let umbral::db::DbPool::Postgres(pool) = &db else {
        panic!("UMBRAL_TEST_POSTGRES_URL must be a Postgres URL");
    };
    let pool = pool.clone();

    // Clean slate — drop the junction first (FKs), then the parents.
    for t in ["m2mpg_post_tags", "m2mpg_post", "m2mpg_tag"] {
        sqlx::query(&format!("DROP TABLE IF EXISTS {t} CASCADE"))
            .execute(&pool)
            .await
            .expect("drop prior table");
    }

    let mut settings = umbral::Settings::from_env().expect("settings");
    settings.database_url = url.clone();
    umbral::App::builder()
        .settings(settings)
        .database("default", db)
        .model::<Tag>()
        .model::<Post>()
        .build()
        .expect("App::build");
    umbral_core::migrate::create_tables_for_tests()
        .await
        .expect("create the test schema on Postgres");

    // The junction's child_id/parent_id must be bigint (referencing the i64
    // PKs) — that's the strict column the text bind used to hit.
    let child_type: (String,) = sqlx::query_as(
        "SELECT data_type FROM information_schema.columns \
         WHERE table_name = 'm2mpg_post_tags' AND column_name = 'child_id'",
    )
    .fetch_one(&pool)
    .await
    .expect("junction child_id column exists");
    assert_eq!(
        child_type.0, "bigint",
        "the M2M junction child_id must be bigint (the strict column the bug hit)"
    );

    // Seed tags + a post with no tags yet.
    for (id, name) in &[(1_i64, "rust"), (2, "web"), (3, "framework")] {
        sqlx::query("INSERT INTO m2mpg_tag (id, name) VALUES ($1, $2)")
            .bind(*id)
            .bind(*name)
            .execute(&pool)
            .await
            .expect("seed tag");
    }
    sqlx::query("INSERT INTO m2mpg_post (id, title) VALUES ($1, $2)")
        .bind(1_i64)
        .bind("p1")
        .execute(&pool)
        .await
        .expect("seed post");

    // The repro: child ids as JSON STRINGS, exactly what an admin form / a REST
    // PATCH of an M2M field sends. Before the fix this returned
    // WriteError::Sqlx("... child_id is of type bigint but expression is of
    // type text"). After the fix the strings coerce to BigInt and the write
    // succeeds.
    let body = json!({ "tags": ["1", "2"] });
    let affected = DynQuerySet::for_meta(&umbral_core::migrate::ModelMeta::for_::<Post>())
        .filter_in_i64("id", &[1])
        .update_json(body.as_object().unwrap())
        .await
        .expect("dynamic M2M update with string child ids must succeed on Postgres");
    assert_eq!(affected, 1, "one parent row updated");

    // The junction now holds (1,1) and (1,2) — read them back as bigint.
    let mut child_ids: Vec<i64> = sqlx::query_scalar(
        "SELECT child_id FROM m2mpg_post_tags WHERE parent_id = $1 ORDER BY child_id",
    )
    .bind(1_i64)
    .fetch_all(&pool)
    .await
    .expect("read junction rows back");
    child_ids.sort_unstable();
    assert_eq!(
        child_ids,
        vec![1, 2],
        "string child ids must persist as the bigint rows {{1, 2}}"
    );
}
