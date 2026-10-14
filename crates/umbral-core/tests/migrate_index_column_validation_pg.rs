//! gaps7 #176 — the Postgres half of the index-column validation matrix.
//!
//! Postgres is the backend that *reported* the bug (`42703`, `ComputeIndexAttrs`,
//! process exits before binding) — the point of the fix is that the ENGINE now
//! refuses the file first, with a message naming the file, the table and the
//! column, so both backends refuse it at the same layer with the same words.
//!
//! `#[ignore]` + `UMBRAL_TEST_POSTGRES_URL`, like every other live-PG test:
//!
//! ```text
//! UMBRAL_TEST_POSTGRES_URL=postgres://… cargo test -p umbral-core \
//!   --test migrate_index_column_validation_pg -- --ignored
//! ```
//!
//! The success half matters as much as the refusal: a valid `unique_together`
//! (and a valid standalone `AddIndex`) must still apply on Postgres and enforce
//! the constraint. The SQLite twin of this file is
//! `tests/migrate_index_column_validation.rs`.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tempfile::TempDir;
use tokio::sync::OnceCell;

use umbral::migrate::{MigrateError, MigrationFile, Operation, Snapshot, diff, run_in};

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "c0p_follow")]
struct FollowPg {
    id: i64,
    follower: i64,
    following: i64,
}

static BOOT: OnceCell<PgPool> = OnceCell::const_new();

/// One boot per test binary. Returns `None` when `UMBRAL_TEST_POSTGRES_URL` is
/// unset, so a stray non-`--ignored` run reports rather than panics.
async fn boot() -> Option<PgPool> {
    let Ok(url) = std::env::var("UMBRAL_TEST_POSTGRES_URL") else {
        return None;
    };
    let pool = BOOT
        .get_or_init(|| async move {
            let pool = PgPool::connect(&url)
                .await
                .expect("connect to the Postgres named by UMBRAL_TEST_POSTGRES_URL");
            let mut settings = umbral::Settings::from_env().expect("settings");
            settings.database_url = url;
            umbral::App::builder()
                .settings(settings)
                .database("default", pool.clone())
                .model::<FollowPg>()
                .build()
                .expect("App::build() should succeed");
            pool
        })
        .await
        .clone();
    Some(pool)
}

/// Drop everything these tests own so a re-run against the same database is a
/// genuine from-scratch apply. Ledger rows are removed best-effort (the
/// tracking table may not exist on a first run).
async fn clean_slate(pool: &PgPool, tables: &[&str], names: &[&str]) {
    for table in tables {
        sqlx::query(&format!("DROP TABLE IF EXISTS \"{table}\""))
            .execute(pool)
            .await
            .expect("drop table");
    }
    for name in names {
        let _ = sqlx::query("DELETE FROM umbral_migrations WHERE name = $1")
            .bind(*name)
            .execute(pool)
            .await;
    }
}

/// The pair `makemigrations` would write for `FollowPg` on `table`, with an
/// optional composite UNIQUE group — real `Column` shapes included.
fn follow_file(id: &str, table: &str, group: Option<&[&str]>) -> MigrationFile {
    let mut snap = Snapshot::current();
    let model = snap
        .models
        .iter_mut()
        .find(|m| m.name == "FollowPg")
        .expect("FollowPg is registered");
    model.table = table.to_string();
    model.unique_together = match group {
        Some(cols) => vec![cols.iter().map(|c| (*c).to_string()).collect()],
        None => Vec::new(),
    };
    let operations = diff(&Snapshot::default(), &snap).expect("a consistent model must diff");
    MigrationFile {
        id: id.to_string(),
        plugin: "app".to_string(),
        depends_on: Vec::new(),
        operations,
        snapshot_after: snap,
        replaces: Vec::new(),
    }
}

/// Damage the group the way the damaged harness migration is damaged: the file
/// names `_id`-suffixed columns the table never had (in the operation and in
/// the snapshot it claims to produce).
fn damage_group(mut file: MigrationFile, group: &[&str]) -> MigrationFile {
    let group: Vec<String> = group.iter().map(|c| (*c).to_string()).collect();
    for op in file.operations.iter_mut() {
        if let Operation::CreateTable {
            unique_together, ..
        } = op
        {
            *unique_together = vec![group.clone()];
        }
        if let Operation::AddIndex { columns, .. } = op {
            *columns = group.clone();
        }
    }
    for model in file.snapshot_after.models.iter_mut() {
        model.unique_together = vec![group.clone()];
    }
    file
}

fn write_migration(dir: &std::path::Path, file: &MigrationFile) {
    let plugin_dir = dir.join(&file.plugin);
    std::fs::create_dir_all(&plugin_dir).expect("create plugin dir");
    std::fs::write(
        plugin_dir.join(format!("{}.json", file.id)),
        serde_json::to_string_pretty(file).expect("serialize"),
    )
    .expect("write migration file");
}

async fn table_exists(pool: &PgPool, table: &str) -> bool {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM information_schema.tables \
         WHERE table_schema = current_schema() AND table_name = $1)",
    )
    .bind(table)
    .fetch_one(pool)
    .await
    .expect("query information_schema")
}

async fn index_definition(pool: &PgPool, index: &str) -> Option<String> {
    sqlx::query_scalar::<_, String>("SELECT indexdef FROM pg_indexes WHERE indexname = $1")
        .bind(index)
        .fetch_optional(pool)
        .await
        .expect("query pg_indexes")
}

async fn insert_follow(
    pool: &PgPool,
    table: &str,
    id: i64,
    follower: i64,
    following: i64,
) -> Result<sqlx::postgres::PgQueryResult, sqlx::Error> {
    sqlx::query(&format!(
        "INSERT INTO \"{table}\" (id, follower, following) VALUES ($1, $2, $3)"
    ))
    .bind(id)
    .bind(follower)
    .bind(following)
    .execute(pool)
    .await
}

// ------------------------------------------------------------------------- //
// Valid groups still apply on Postgres — the success path.                  //
// ------------------------------------------------------------------------- //

async fn valid_unique_together_applies(pool: &PgPool) {
    clean_slate(pool, &["pg_valid_follow"], &["0001_create_pg_valid_follow"]).await;

    let dir = TempDir::new().expect("tempdir");
    write_migration(
        dir.path(),
        &follow_file(
            "0001_create_pg_valid_follow",
            "pg_valid_follow",
            Some(&["follower", "following"]),
        ),
    );

    let applied = run_in(dir.path())
        .await
        .expect("a migration whose group names real columns must apply on Postgres");
    assert_eq!(applied, 1);
    assert!(table_exists(pool, "pg_valid_follow").await);

    let def = index_definition(pool, "uniq_pg_valid_follow_follower_following")
        .await
        .expect("the composite UNIQUE index must exist");
    assert!(
        def.contains("(follower, following)"),
        "the index must be over the two real columns; got: {def}"
    );

    // Behavioural proof: distinct pairs insert, a repeat is rejected.
    insert_follow(pool, "pg_valid_follow", 1, 1, 1)
        .await
        .expect("first pair");
    insert_follow(pool, "pg_valid_follow", 2, 1, 2)
        .await
        .expect("second, distinct pair");
    let dup = insert_follow(pool, "pg_valid_follow", 3, 1, 1).await;
    let err = dup.expect_err("a repeated pair must violate the composite UNIQUE");
    assert!(
        err.to_string().contains("duplicate key"),
        "expected a unique violation, got: {err}"
    );

    clean_slate(pool, &["pg_valid_follow"], &["0001_create_pg_valid_follow"]).await;
}

async fn valid_add_index_applies(pool: &PgPool) {
    clean_slate(
        pool,
        &["pg_idx_follow"],
        &["0001_create_pg_idx_follow", "0002_add_index_pg_idx_follow"],
    )
    .await;

    let dir = TempDir::new().expect("tempdir");
    let create = follow_file("0001_create_pg_idx_follow", "pg_idx_follow", None);
    let mut snapshot = create.snapshot_after.clone();
    for model in snapshot.models.iter_mut() {
        model.unique_together = vec![vec!["follower".to_string(), "following".to_string()]];
    }
    let add = MigrationFile {
        id: "0002_add_index_pg_idx_follow".to_string(),
        plugin: "app".to_string(),
        depends_on: Vec::new(),
        operations: vec![Operation::AddIndex {
            table: "pg_idx_follow".to_string(),
            columns: vec!["follower".to_string(), "following".to_string()],
            unique: true,
        }],
        snapshot_after: snapshot,
        replaces: Vec::new(),
    };
    write_migration(dir.path(), &create);
    write_migration(dir.path(), &add);

    let applied = run_in(dir.path()).await.expect("both files must apply");
    assert_eq!(applied, 2);

    let def = index_definition(pool, "uniq_pg_idx_follow_follower_following")
        .await
        .expect("the AddIndex must have created the index");
    assert!(
        def.contains("(follower, following)"),
        "the index must be over the two real columns; got: {def}"
    );

    clean_slate(
        pool,
        &["pg_idx_follow"],
        &["0001_create_pg_idx_follow", "0002_add_index_pg_idx_follow"],
    )
    .await;
}

// ------------------------------------------------------------------------- //
// Invalid groups are refused by the ENGINE, not by Postgres' `42703`.       //
// ------------------------------------------------------------------------- //

async fn invalid_unique_together_is_refused(pool: &PgPool) {
    clean_slate(pool, &["pg_bad_follow"], &["0001_create_pg_bad_follow"]).await;

    let dir = TempDir::new().expect("tempdir");
    write_migration(
        dir.path(),
        &damage_group(
            follow_file("0001_create_pg_bad_follow", "pg_bad_follow", None),
            &["follower_id", "following_id"],
        ),
    );

    let err = run_in(dir.path())
        .await
        .expect_err("the engine must refuse the file before Postgres can reject its SQL");
    match &err {
        MigrateError::InvalidIndexTarget {
            file,
            kind,
            table,
            missing,
            ..
        } => {
            assert!(file.contains("0001_create_pg_bad_follow.json"), "{file}");
            assert_eq!(kind, "unique_together");
            assert_eq!(table, "pg_bad_follow");
            assert_eq!(
                missing,
                &vec!["follower_id".to_string(), "following_id".to_string()]
            );
            // The refusal is the ENGINE's, not Postgres' `42703`.
            assert!(
                !format!("{err:?}").contains("Sqlx"),
                "must not be a sqlx error: {err:?}"
            );
        }
        other => panic!("expected InvalidIndexTarget, got {other:?}"),
    }

    assert!(
        !table_exists(pool, "pg_bad_follow").await,
        "a refused migration must not create anything on Postgres either"
    );

    clean_slate(pool, &["pg_bad_follow"], &["0001_create_pg_bad_follow"]).await;
}

async fn invalid_add_index_is_refused(pool: &PgPool) {
    clean_slate(
        pool,
        &["pg_badidx_follow"],
        &[
            "0001_create_pg_badidx_follow",
            "0002_add_index_pg_badidx_follow",
        ],
    )
    .await;

    let dir = TempDir::new().expect("tempdir");
    let create = follow_file("0001_create_pg_badidx_follow", "pg_badidx_follow", None);
    let add = damage_group(
        MigrationFile {
            id: "0002_add_index_pg_badidx_follow".to_string(),
            plugin: "app".to_string(),
            depends_on: Vec::new(),
            operations: vec![Operation::AddIndex {
                table: "pg_badidx_follow".to_string(),
                columns: vec!["follower".to_string(), "following".to_string()],
                unique: true,
            }],
            snapshot_after: create.snapshot_after.clone(),
            replaces: Vec::new(),
        },
        &["follower_id", "following_id"],
    );
    write_migration(dir.path(), &create);
    write_migration(dir.path(), &add);

    let err = run_in(dir.path())
        .await
        .expect_err("an AddIndex naming columns the table does not have must be refused");
    match &err {
        MigrateError::InvalidIndexTarget { kind, table, .. } => {
            assert_eq!(kind, "AddIndex");
            assert_eq!(table, "pg_badidx_follow");
        }
        other => panic!("expected InvalidIndexTarget, got {other:?}"),
    }

    assert!(
        !table_exists(pool, "pg_badidx_follow").await,
        "no file in the set may be applied when one of them is invalid"
    );

    clean_slate(
        pool,
        &["pg_badidx_follow"],
        &[
            "0001_create_pg_badidx_follow",
            "0002_add_index_pg_badidx_follow",
        ],
    )
    .await;
}

// ------------------------------------------------------------------------- //
// One test drives all four phases.                                      //
//                                                                        //
// `App::build()` publishes the pool in a process-wide `OnceLock`, so the  //
// pool can only ever be created on ONE runtime — and a sqlx pool may not  //
// be used from a runtime other than the one that built it (its background //
// tasks die with that runtime and a later acquire hangs; that is exactly  //
// what four `#[tokio::test]`s sharing one boot did here). One test, four  //
// phases, in order.                                                       //
// ------------------------------------------------------------------------- //

#[tokio::test]
#[ignore = "needs UMBRAL_TEST_POSTGRES_URL"]
async fn postgres_refuses_invalid_index_columns_and_still_applies_valid_ones() {
    let Some(pool) = boot().await else {
        eprintln!("skipping: UMBRAL_TEST_POSTGRES_URL not set");
        return;
    };
    // Refusals first, so a hang or panic in the success path can never be
    // mistaken for "the invalid case passed".
    invalid_unique_together_is_refused(&pool).await;
    invalid_add_index_is_refused(&pool).await;
    valid_unique_together_applies(&pool).await;
    valid_add_index_applies(&pool).await;
}
