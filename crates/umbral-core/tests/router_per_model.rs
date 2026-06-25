//! The router receives each query's `ModelMeta`, so a custom router can route
//! different models to different databases off the model's identity. The
//! read/write-split test ignores the meta; this one keys on it.

#![allow(dead_code)]

use umbral::db::{Alias, DatabaseRouter, RouteContext};
use umbral::migrate::ModelMeta;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "rt_alpha")]
pub struct Alpha {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "rt_beta")]
pub struct Beta {
    pub id: i64,
    pub name: String,
}

/// `rt_alpha` -> db_a, `rt_beta` -> db_b, everything else -> default.
fn alias_of(table: &str) -> &'static str {
    match table {
        "rt_alpha" => "db_a",
        "rt_beta" => "db_b",
        _ => "default",
    }
}

struct PerModelRouter;
impl DatabaseRouter for PerModelRouter {
    fn db_for_read(&self, m: &ModelMeta, _c: &RouteContext) -> Alias {
        Alias::new(alias_of(&m.table))
    }
    fn db_for_write(&self, m: &ModelMeta, _c: &RouteContext) -> Alias {
        Alias::new(alias_of(&m.table))
    }
}

async fn pool_with_table(table: &str) -> sqlx::SqlitePool {
    let pool = umbral_core::db::connect_sqlite("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query(&format!(
        "CREATE TABLE {table} (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)"
    ))
    .execute(&pool)
    .await
    .unwrap();
    pool
}

#[tokio::test(flavor = "multi_thread")]
async fn router_routes_each_model_to_its_own_database() {
    let default = umbral_core::db::connect_sqlite("sqlite::memory:")
        .await
        .unwrap();
    let db_a = pool_with_table("rt_alpha").await;
    let db_b = pool_with_table("rt_beta").await;

    umbral::App::builder()
        .settings(umbral::Settings::from_env().expect("settings"))
        .database("default", default)
        .database("db_a", db_a.clone())
        .database("db_b", db_b.clone())
        .router(PerModelRouter)
        .model::<Alpha>()
        .model::<Beta>()
        .build()
        .unwrap();

    // Each write lands in the model's own database.
    Alpha::objects()
        .create(Alpha {
            id: 0,
            name: "alpha1".into(),
        })
        .await
        .unwrap();
    Beta::objects()
        .create(Beta {
            id: 0,
            name: "beta1".into(),
        })
        .await
        .unwrap();

    let a_in_db_a: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM rt_alpha")
        .fetch_one(&db_a)
        .await
        .unwrap();
    let b_in_db_b: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM rt_beta")
        .fetch_one(&db_b)
        .await
        .unwrap();
    assert_eq!(
        (a_in_db_a, b_in_db_b),
        (1, 1),
        "each model's write routed to its own database"
    );

    // Reads route per-model too, returning that model's rows.
    let alphas = Alpha::objects().fetch().await.unwrap();
    assert_eq!(alphas.len(), 1);
    assert_eq!(alphas[0].name, "alpha1");
    let betas = Beta::objects().fetch().await.unwrap();
    assert_eq!(betas.len(), 1);
    assert_eq!(betas[0].name, "beta1");
}
