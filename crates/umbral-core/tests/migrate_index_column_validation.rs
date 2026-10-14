//! gaps7 #176 — an index group must name columns the target table actually
//! has, and the engine must refuse the migration on BOTH backends when it
//! doesn't.
//!
//! The finding: a `unique_together` group naming columns that do not exist
//! (`["follower_id", "following_id"]` where the table has `follower` /
//! `following`) makes Postgres and SQLite disagree. Postgres rejects the
//! rendered `CREATE UNIQUE INDEX ... ("follower_id", ...)` with `42703` and the
//! process exits before binding — correct. SQLite reads an unknown
//! *double-quoted* token as a **string literal**, so it builds a unique index
//! over two constants: a constant key, at most one row can ever satisfy it, and
//! it reports success. A fresh SQLite build of such an app is silently corrupt.
//!
//! These tests pin both halves of the fix, on SQLite — the generation half
//! (`diff` refuses a model whose declared group names a field it does not have)
//! and the apply half (`run_in` refuses a migration file whose operations would
//! create an index over a column the file itself says the table lacks).
//!
//! They pin the SUCCESS path just as hard, because it is the one every existing
//! app depends on: a valid `unique_together` (and a valid standalone `AddIndex`)
//! still applies, and still enforces uniqueness.
//!
//! The Postgres half of the same matrix lives in
//! `tests/migrate_index_column_validation_pg.rs` (`#[ignore]`, driven with
//! `UMBRAL_TEST_POSTGRES_URL`).

#![allow(dead_code)]

use std::path::Path;

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tempfile::TempDir;
use tokio::sync::OnceCell;

use umbral::migrate::{MigrateError, MigrationFile, Operation, Snapshot, diff, run_in};

/// The table under test: a follow edge with a composite UNIQUE on the pair of
/// FK columns. Column names are the FIELD names (`follower`, `following`) —
/// which is exactly what the damaged harness migration got wrong.
#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "c0_follow")]
struct Follow {
    id: i64,
    follower: i64,
    following: i64,
}

static BOOT: OnceCell<SqlitePool> = OnceCell::const_new();

/// One boot per test binary (the model registry, pool, and backend live in
/// process-wide `OnceLock`s). The returned handle is a clone of the pool —
/// sqlx pools are cheap handles over one connection pool.
async fn boot() -> SqlitePool {
    BOOT.get_or_init(|| async {
        let settings =
            umbral::Settings::from_env().expect("figment defaults always load in a test env");
        let pool = umbral::db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite should always connect");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Follow>()
            .build()
            .expect("App::build() should succeed");
        pool
    })
    .await
    .clone()
}

/// The snapshot the engine would write for the `Follow` model, optionally with
/// a composite UNIQUE group on `table` — built by renaming/re-grouping the
/// registered model, then diffing, so the operation and the snapshot are the
/// exact pair `makemigrations` produces (correct `Column` shapes included).
fn follow_file(id: &str, table: &str, group: Option<&[&str]>) -> MigrationFile {
    let mut snap = Snapshot::current();
    let model = snap
        .models
        .iter_mut()
        .find(|m| m.name == "Follow")
        .expect("Follow is registered");
    model.table = table.to_string();
    model.unique_together = match group {
        Some(cols) => vec![cols.iter().map(|c| (*c).to_string()).collect()],
        None => Vec::new(),
    };
    let operations = diff(&Snapshot::default(), &snap)
        .expect("a model whose groups name real fields must diff cleanly");
    MigrationFile {
        id: id.to_string(),
        plugin: "app".to_string(),
        depends_on: Vec::new(),
        operations,
        snapshot_after: snap,
        replaces: Vec::new(),
    }
}

/// Damage a file the way the `umbra→umbral` rename damaged the harness
/// migration: the declared group still names `_id`-suffixed columns the table
/// never had — in the operation AND in the snapshot the file claims to produce.
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

/// The two snapshots of the reviewer's shape (fix-round 1): the model as
/// declared, then the same model with a **new** column, a group naming that new
/// column, and an existing column altered — all on one table, so `diff_columns`
/// emits an `AlterColumn` whose `new_columns` deliberately do NOT contain the
/// new column (it is added by the follow-on `AddColumn`).
fn add_grouped_column_and_alter(table: &str) -> (Snapshot, Snapshot) {
    let mut prev = Snapshot::current();
    prev.models
        .iter_mut()
        .find(|m| m.name == "Follow")
        .expect("Follow is registered")
        .table = table.to_string();

    let mut curr = prev.clone();
    let model = curr
        .models
        .iter_mut()
        .find(|m| m.name == "Follow")
        .expect("Follow is registered");

    // A new nullable text column (nullable because a NOT NULL add on a
    // populated table is refused without a default).
    let mut slug = model
        .fields
        .iter()
        .find(|c| c.name == "following")
        .expect("following column")
        .clone();
    slug.name = "slug".to_string();
    slug.ty = umbral::orm::SqlType::Text;
    slug.nullable = true;
    model.fields.push(slug);

    // ...grouped, so the group names a column the old table does not have.
    model.unique_together = vec![vec!["follower".to_string(), "slug".to_string()]];

    // ...and an existing column altered in the same diff.
    model
        .fields
        .iter_mut()
        .find(|c| c.name == "following")
        .expect("following column")
        .nullable = true;

    (prev, curr)
}

fn write_migration(dir: &Path, file: &MigrationFile) {
    let plugin_dir = dir.join(&file.plugin);
    std::fs::create_dir_all(&plugin_dir).expect("create plugin dir");
    let path = plugin_dir.join(format!("{}.json", file.id));
    std::fs::write(
        &path,
        serde_json::to_string_pretty(file).expect("serialize"),
    )
    .expect("write migration file");
}

async fn table_exists(pool: &SqlitePool, table: &str) -> bool {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
    )
    .bind(table)
    .fetch_one(pool)
    .await
    .expect("query sqlite_master")
        > 0
}

/// The columns a SQLite index is built over, in order.
async fn index_columns(pool: &SqlitePool, index: &str) -> Vec<String> {
    sqlx::query_scalar::<_, String>(&format!(
        "SELECT name FROM pragma_index_info('{index}') ORDER BY seqno"
    ))
    .fetch_all(pool)
    .await
    .expect("pragma index_info")
}

// ------------------------------------------------------------------------- //
// Generation half: `diff` refuses a declared group naming a missing field.  //
// ------------------------------------------------------------------------- //

#[tokio::test]
async fn diff_refuses_a_group_that_names_a_field_the_model_does_not_have() {
    boot().await;

    let mut snap = Snapshot::current();
    snap.models
        .iter_mut()
        .find(|m| m.name == "Follow")
        .expect("Follow is registered")
        .unique_together = vec![vec!["follower_id".to_string(), "following_id".to_string()]];

    let err = diff(&Snapshot::default(), &snap)
        .expect_err("a group naming fields the model does not have must be refused");
    match &err {
        MigrateError::InvalidDeclaredIndex {
            model,
            table,
            kind,
            missing,
            available,
            ..
        } => {
            assert_eq!(model, "Follow");
            assert_eq!(table, "c0_follow");
            assert_eq!(kind, "unique_together");
            assert_eq!(
                missing,
                &vec!["follower_id".to_string(), "following_id".to_string()]
            );
            assert!(
                available.contains(&"follower".to_string())
                    && available.contains(&"following".to_string()),
                "the error must name the columns the table DOES have; got {available:?}"
            );
        }
        other => panic!("expected InvalidDeclaredIndex, got {other:?}"),
    }

    let msg = err.to_string();
    for needle in ["Follow", "follower_id", "unique_together", "follower"] {
        assert!(
            msg.contains(needle),
            "the message must mention `{needle}`; got:\n{msg}"
        );
    }
}

#[tokio::test]
async fn diff_still_accepts_a_group_that_names_real_fields() {
    boot().await;

    let file = follow_file(
        "0001_create_c0_follow",
        "c0_follow",
        Some(&["follower", "following"]),
    );
    match &file.operations[..] {
        [
            Operation::CreateTable {
                table,
                unique_together,
                ..
            },
        ] => {
            assert_eq!(table, "c0_follow");
            assert_eq!(
                unique_together,
                &vec![vec!["follower".to_string(), "following".to_string()]],
                "the valid group must survive into the operation"
            );
        }
        other => panic!("expected one CreateTable, got {other:?}"),
    }
}

// ------------------------------------------------------------------------- //
// Apply half (SQLite): the invalid file is refused, the valid one applies.  //
// ------------------------------------------------------------------------- //

#[tokio::test]
async fn invalid_unique_together_is_refused_and_creates_nothing() {
    let pool = boot().await;

    let dir = TempDir::new().expect("tempdir");
    let file = damage_group(
        follow_file("0001_create_c1_follow", "c1_follow", None),
        &["follower_id", "following_id"],
    );
    write_migration(dir.path(), &file);

    let err = run_in(dir.path())
        .await
        .expect_err("a migration naming columns the table does not have must be refused");
    match &err {
        MigrateError::InvalidIndexTarget {
            file,
            kind,
            table,
            missing,
            available,
            ..
        } => {
            assert!(
                file.contains("0001_create_c1_follow.json"),
                "the error must name the migration file; got `{file}`"
            );
            assert_eq!(kind, "unique_together");
            assert_eq!(table, "c1_follow");
            assert_eq!(
                missing,
                &vec!["follower_id".to_string(), "following_id".to_string()]
            );
            assert!(available.contains(&"follower".to_string()));
        }
        other => panic!("expected InvalidIndexTarget, got {other:?}"),
    }

    // The error must name the table, the column, and the file.
    let msg = err.to_string();
    for needle in [
        "0001_create_c1_follow.json",
        "c1_follow",
        "follower_id",
        "following_id",
        "follower",
    ] {
        assert!(
            msg.contains(needle),
            "the message must mention `{needle}`; got:\n{msg}"
        );
    }

    // Refused means refused: no table, no tracking row — nothing half-applied.
    assert!(
        !table_exists(&pool, "c1_follow").await,
        "a refused migration must not have created its table"
    );
    let recorded: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM umbral_migrations WHERE name = ?")
        .bind("0001_create_c1_follow")
        .fetch_one(&pool)
        .await
        .expect("query ledger");
    assert_eq!(recorded, 0, "a refused migration must not be recorded");
}

#[tokio::test]
async fn valid_unique_together_still_applies_on_sqlite_and_enforces_uniqueness() {
    let pool = boot().await;

    let dir = TempDir::new().expect("tempdir");
    let file = follow_file(
        "0001_create_c0_follow_valid",
        "c0_valid_follow",
        Some(&["follower", "following"]),
    );
    write_migration(dir.path(), &file);

    let applied = run_in(dir.path())
        .await
        .expect("a migration whose group names real columns must apply");
    assert_eq!(applied, 1);
    assert!(table_exists(&pool, "c0_valid_follow").await);

    // The index is over the two REAL columns — not over string constants.
    let index = "uniq_c0_valid_follow_follower_following";
    assert_eq!(
        index_columns(&pool, index).await,
        vec!["follower".to_string(), "following".to_string()],
        "the UNIQUE index must be built over the named columns"
    );

    // …and it behaves as a composite UNIQUE: distinct pairs insert, a repeat
    // of an existing pair is rejected.
    for (follower, following) in [(1i64, 1i64), (1, 2)] {
        sqlx::query("INSERT INTO \"c0_valid_follow\" (id, follower, following) VALUES (?, ?, ?)")
            .bind(follower * 10 + following)
            .bind(follower)
            .bind(following)
            .execute(&pool)
            .await
            .expect("a distinct pair must insert");
    }
    let dup =
        sqlx::query("INSERT INTO \"c0_valid_follow\" (id, follower, following) VALUES (?, ?, ?)")
            .bind(99)
            .bind(1)
            .bind(1)
            .execute(&pool)
            .await;
    let err = dup.expect_err("a repeated pair must violate the composite UNIQUE");
    assert!(
        err.to_string().to_lowercase().contains("unique"),
        "expected a UNIQUE violation, got: {err}"
    );
}

// ------------------------------------------------------------------------- //
// The standalone `AddIndex` op — the other index-creating shape.            //
// ------------------------------------------------------------------------- //

#[tokio::test]
async fn valid_add_index_still_applies_on_sqlite() {
    let pool = boot().await;

    let dir = TempDir::new().expect("tempdir");
    let create = follow_file("0001_create_c2_follow", "c2_follow", None);
    let mut snapshot = create.snapshot_after.clone();
    let add = MigrationFile {
        id: "0002_add_index_c2_follow".to_string(),
        plugin: "app".to_string(),
        depends_on: Vec::new(),
        operations: vec![Operation::AddIndex {
            table: "c2_follow".to_string(),
            columns: vec!["follower".to_string(), "following".to_string()],
            unique: true,
        }],
        snapshot_after: {
            for model in snapshot.models.iter_mut() {
                model.unique_together = vec![vec!["follower".to_string(), "following".to_string()]];
            }
            snapshot
        },
        replaces: Vec::new(),
    };
    write_migration(dir.path(), &create);
    write_migration(dir.path(), &add);

    let applied = run_in(dir.path()).await.expect("both files must apply");
    assert_eq!(applied, 2);

    assert_eq!(
        index_columns(&pool, "uniq_c2_follow_follower_following").await,
        vec!["follower".to_string(), "following".to_string()]
    );
}

#[tokio::test]
async fn invalid_add_index_is_refused_on_sqlite() {
    let pool = boot().await;

    let dir = TempDir::new().expect("tempdir");
    let create = follow_file("0001_create_c3_follow", "c3_follow", None);
    let add = damage_group(
        MigrationFile {
            id: "0002_add_index_c3_follow".to_string(),
            plugin: "app".to_string(),
            depends_on: Vec::new(),
            operations: vec![Operation::AddIndex {
                table: "c3_follow".to_string(),
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
        MigrateError::InvalidIndexTarget {
            kind,
            table,
            missing,
            ..
        } => {
            assert_eq!(kind, "AddIndex");
            assert_eq!(table, "c3_follow");
            assert_eq!(
                missing,
                &vec!["follower_id".to_string(), "following_id".to_string()]
            );
        }
        other => panic!("expected InvalidIndexTarget, got {other:?}"),
    }

    // The whole plugin's file set is parsed before anything applies, so the
    // earlier (valid) CreateTable must not have run either.
    assert!(
        !table_exists(&pool, "c3_follow").await,
        "no file in the plugin's set may be applied when one of them is invalid"
    );
}

// ------------------------------------------------------------------------- //
// Fix round 1 — a grouped NEW column alongside an alter on the same table.  //
//                                                                           //
// `diff_columns` shapes an `AlterColumn`'s `new_columns` like the PREVIOUS   //
// table (the SAME diff's `AddColumn` introduces the new column afterwards),  //
// while `diff_indexes` emits the standalone `AddIndex` that builds the new   //
// group. Carrying the current group on the `AlterColumn` too made the        //
// operation name a column that does not exist at that instant, which the     //
// read-path check rightly refuses — so the engine must not emit it. This is  //
// the reviewer's reproduction: without the filter in `diff_columns` the file //
// below is refused on BOTH backends.                                         //
// ------------------------------------------------------------------------- //

#[tokio::test]
async fn a_new_grouped_column_alongside_an_alter_still_applies() {
    let pool = boot().await;

    let (prev, curr) = add_grouped_column_and_alter("c4_alter_follow");
    let create = MigrationFile {
        id: "0001_create_c4_alter_follow".to_string(),
        plugin: "app".to_string(),
        depends_on: Vec::new(),
        operations: diff(&Snapshot::default(), &prev).expect("create table"),
        snapshot_after: prev.clone(),
        replaces: Vec::new(),
    };
    let alter = MigrationFile {
        id: "0002_alter_c4_alter_follow_add_slug".to_string(),
        plugin: "app".to_string(),
        depends_on: Vec::new(),
        operations: diff(&prev, &curr).expect("the model change must diff cleanly"),
        snapshot_after: curr,
        replaces: Vec::new(),
    };

    // The shape under test is really present: an `AlterColumn` on the table…
    let alter_col = alter
        .operations
        .iter()
        .find_map(|op| match op {
            Operation::AlterColumn {
                table,
                column,
                unique_together,
                ..
            } if table == "c4_alter_follow" => Some((column.clone(), unique_together.clone())),
            _ => None,
        })
        .expect("the diff must emit an AlterColumn for the altered column");
    assert_eq!(alter_col.0, "following");
    assert!(
        alter_col.1.is_empty(),
        "the AlterColumn must not carry a group naming the to-be-added `slug` — the \
         follow-on AddIndex owns it; got {:?}",
        alter_col.1
    );
    // …and the group survives as its own `AddIndex`.
    assert!(
        alter.operations.iter().any(|op| matches!(
            op,
            Operation::AddIndex { table, columns, unique: true }
                if table == "c4_alter_follow"
                    && columns == &vec!["follower".to_string(), "slug".to_string()]
        )),
        "the new group must be created by an AddIndex: {:?}",
        alter.operations
    );

    let dir = TempDir::new().expect("tempdir");
    write_migration(dir.path(), &create);
    write_migration(dir.path(), &alter);

    let applied = run_in(dir.path())
        .await
        .expect("a grouped new column alongside an alter must apply, not be refused");
    assert_eq!(applied, 2);
    assert!(table_exists(&pool, "c4_alter_follow").await);

    // The constraint is real and over the RIGHT columns…
    assert_eq!(
        index_columns(&pool, "uniq_c4_alter_follow_follower_slug").await,
        vec!["follower".to_string(), "slug".to_string()]
    );
    // …and enforces exactly the pair (not a narrower or constant key).
    for (id, follower, slug) in [(1i64, 1i64, "a"), (2, 1, "b")] {
        sqlx::query(
            "INSERT INTO \"c4_alter_follow\" (id, follower, following, slug) VALUES (?, ?, ?, ?)",
        )
        .bind(id)
        .bind(follower)
        .bind(follower)
        .bind(slug)
        .execute(&pool)
        .await
        .unwrap_or_else(|e| panic!("({follower}, {slug}) must insert: {e}"));
    }
    let dup = sqlx::query(
        "INSERT INTO \"c4_alter_follow\" (id, follower, following, slug) VALUES (3, 1, 1, 'a')",
    )
    .execute(&pool)
    .await;
    let err = dup.expect_err("the repeated (follower, slug) pair must violate the UNIQUE");
    assert!(
        err.to_string().to_lowercase().contains("unique"),
        "expected a UNIQUE violation, got: {err}"
    );
}
