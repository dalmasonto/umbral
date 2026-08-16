//! features #89b — read a NATIVE Postgres enum column through the dynamic path
//! (what umbral-rest and `transferdata` use), when the model represents the
//! column as a `TEXT`-backed `#[umbral(choices)]` enum.
//!
//! A native `CREATE TYPE ... AS ENUM` column fails sqlx's CHECKED
//! `try_get::<String>` (its OID isn't a text OID), even though the wire value
//! IS the label. The `DynQuerySet` read path now falls back to the unchecked
//! decode for a text column, so a choices column recovered from a native enum
//! reads its label straight from an un-migrated source DB.
//!
//! Gated on `UMBRAL_TEST_POSTGRES_URL`; own binary (App::build is process-global).

use umbral::migrate::ModelMeta;
use umbral::orm::DynQuerySet;
use umbral::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Choices)]
#[choices(rename_all = "SCREAMING_SNAKE_CASE")]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EnsKind {
    Alias,
    EnsName,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]
#[umbral(table = "native_enum_probe")]
pub struct EnumProbe {
    pub id: i64,
    #[umbral(choices)]
    pub kind: EnsKind,
}

#[tokio::test]
#[ignore = "needs UMBRAL_TEST_POSTGRES_URL pointing at a Postgres server"]
async fn native_enum_column_decodes_via_dynamic_path() {
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

    // A NATIVE enum type + a table using it — NOT umbral's TEXT+CHECK shape.
    // This is exactly the un-migrated source a Prisma/Django DB presents.
    for stmt in [
        "DROP TABLE IF EXISTS native_enum_probe",
        "DROP TYPE IF EXISTS ens_kind_native",
        "CREATE TYPE ens_kind_native AS ENUM ('ALIAS', 'ENS_NAME')",
        "CREATE TABLE native_enum_probe (id BIGINT PRIMARY KEY, kind ens_kind_native NOT NULL)",
        "INSERT INTO native_enum_probe (id, kind) VALUES (1, 'ENS_NAME'), (2, 'ALIAS')",
    ] {
        sqlx::query(stmt).execute(&pool).await.expect("pg setup");
    }

    // Register the model so its ModelMeta (kind = TEXT-backed choices) exists.
    let mut settings = umbral::Settings::from_env().expect("settings");
    settings.database_url = url.clone();
    umbral::App::builder()
        .settings(settings)
        .database("default", db)
        .model::<EnumProbe>()
        .build()
        .expect("App::build");

    // Read through the DYNAMIC path (umbral-rest / transferdata's path). The
    // `kind` column is a native enum in the DB but TEXT in the model; the read
    // must recover the label, not 500 on a type mismatch.
    let meta = ModelMeta::for_::<EnumProbe>();
    let rows = DynQuerySet::for_meta(&meta)
        .order_by_col("id", false)
        .fetch_as_json_on(&pool_dbpool(&url).await)
        .await
        .expect("dynamic read of a native enum column must not error");

    assert_eq!(rows.len(), 2, "both rows read");
    assert_eq!(
        rows[0].get("kind").and_then(|v| v.as_str()),
        Some("ENS_NAME"),
        "native enum label decodes via the choices/TEXT model"
    );
    assert_eq!(rows[1].get("kind").and_then(|v| v.as_str()), Some("ALIAS"),);

    sqlx::query("DROP TABLE IF EXISTS native_enum_probe")
        .execute(&pool)
        .await
        .ok();
    sqlx::query("DROP TYPE IF EXISTS ens_kind_native")
        .execute(&pool)
        .await
        .ok();
}

/// Re-open a `DbPool` for the explicit-pool read (the app owns the first one).
async fn pool_dbpool(url: &str) -> umbral::db::DbPool {
    umbral::db::connect(url).await.expect("connect")
}
