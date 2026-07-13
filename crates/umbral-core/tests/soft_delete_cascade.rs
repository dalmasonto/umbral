//! Soft-delete cascade (gaps3 #53).
//!
//! `on_delete = "cascade"` is a DDL clause — the database fires it on a real
//! `DELETE`. A soft delete is an `UPDATE`, so the database never fires, and a
//! soft-deleted parent used to leave its children behind as **live rows**.
//!
//! The bug was easy to miss because reads *through* the parent hide the children.
//! So these tests deliberately query the children **directly**, which is where
//! the orphans were visible: still answering their own queries, still counting.

use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqlitePoolOptions;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "club", soft_delete)]
pub struct Club {
    pub id: i64,
    pub name: String,
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Cascade child of Club.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "team", soft_delete)]
pub struct Team {
    pub id: i64,
    #[umbral(on_delete = "cascade")]
    pub club: umbral::orm::ForeignKey<Club>,
    pub name: String,
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Cascade GRANDchild — proves the cascade recurses, not just one level.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "player", soft_delete)]
pub struct Player {
    pub id: i64,
    #[umbral(on_delete = "cascade")]
    pub team: umbral::orm::ForeignKey<Team>,
    pub name: String,
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// NOT a cascade child — `set_null` means it outlives its parent, so the cascade
/// must leave it alone.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "invoice", soft_delete)]
pub struct Invoice {
    pub id: i64,
    #[umbral(on_delete = "set_null")]
    pub club: Option<umbral::orm::ForeignKey<Club>>,
    pub amount: i64,
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

async fn boot() {
    static ONCE: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    ONCE.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let pool = SqlitePoolOptions::new()
            .connect("sqlite::memory:")
            .await
            .expect("pool");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Club>()
            .model::<Team>()
            .model::<Player>()
            .model::<Invoice>()
            .build()
            .expect("App::build");
        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
    })
    .await;
}

fn lock() -> &'static tokio::sync::Mutex<()> {
    static L: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    &L
}

/// Seed one club → one team → one player, plus an invoice. Returns the club id.
async fn seed(club_name: &str) -> i64 {
    let pool = umbral::db::pool();
    let club = sqlx::query("INSERT INTO club (name) VALUES (?) RETURNING id")
        .bind(club_name)
        .fetch_one(&pool)
        .await
        .expect("club");
    let club_id: i64 = sqlx::Row::get(&club, 0);
    let team = sqlx::query("INSERT INTO team (club, name) VALUES (?, 'first xi') RETURNING id")
        .bind(club_id)
        .fetch_one(&pool)
        .await
        .expect("team");
    let team_id: i64 = sqlx::Row::get(&team, 0);
    sqlx::query("INSERT INTO player (team, name) VALUES (?, 'ada')")
        .bind(team_id)
        .execute(&pool)
        .await
        .expect("player");
    sqlx::query("INSERT INTO invoice (club, amount) VALUES (?, 100)")
        .bind(club_id)
        .execute(&pool)
        .await
        .expect("invoice");
    club_id
}

/// Live rows of a table, queried DIRECTLY — the orphans were only visible here.
async fn live(table: &str) -> i64 {
    let pool = umbral::db::pool();
    let row = sqlx::query(&format!(
        "SELECT COUNT(*) FROM {table} WHERE deleted_at IS NULL"
    ))
    .fetch_one(&pool)
    .await
    .expect("count");
    sqlx::Row::get(&row, 0)
}

/// **The bug.** Soft-deleting a club must soft-delete its teams AND their
/// players. Before this fix both stayed live: hidden when traversed from the
/// club, but still answering their own queries.
#[tokio::test]
async fn soft_deleting_a_parent_cascades_to_children_and_grandchildren() {
    let _g = lock().lock().await;
    boot().await;
    let club_id = seed("cascade-club").await;

    let (t0, p0) = (live("team").await, live("player").await);
    assert!(t0 >= 1 && p0 >= 1, "seeded rows are live to start");

    Club::objects()
        .filter(club::ID.eq(club_id))
        .delete()
        .await
        .expect("soft delete club");

    assert_eq!(
        live("team").await,
        t0 - 1,
        "the club's team must be soft-deleted with it (cascade child)",
    );
    assert_eq!(
        live("player").await,
        p0 - 1,
        "the team's player must be soft-deleted too — the cascade recurses",
    );
}

/// A non-cascade FK (`set_null`) means the child outlives its parent. The
/// cascade must not touch it — over-cascading is as wrong as under-cascading.
#[tokio::test]
async fn a_non_cascade_child_is_left_alone() {
    let _g = lock().lock().await;
    boot().await;
    let club_id = seed("invoice-club").await;
    let before = live("invoice").await;

    Club::objects()
        .filter(club::ID.eq(club_id))
        .delete()
        .await
        .expect("soft delete");

    assert_eq!(
        live("invoice").await,
        before,
        "`on_delete = set_null` does not cascade — the invoice must survive",
    );
}

/// Restore must bring back exactly what the cascade took. Without this, the fix
/// would trade an orphan bug for an unrecoverable-delete bug.
#[tokio::test]
async fn restoring_the_parent_restores_the_cascaded_descendants() {
    let _g = lock().lock().await;
    boot().await;
    let club_id = seed("restore-club").await;
    let (t0, p0) = (live("team").await, live("player").await);

    Club::objects()
        .filter(club::ID.eq(club_id))
        .delete()
        .await
        .expect("soft delete");
    assert_eq!(live("team").await, t0 - 1, "cascaded down");

    umbral::orm::DynQuerySet::for_meta(&umbral::migrate::ModelMeta::for_::<Club>())
        .filter_in_i64("id", &[club_id])
        .restore()
        .await
        .expect("restore club");

    assert_eq!(live("team").await, t0, "the team comes back with its club");
    assert_eq!(live("player").await, p0, "and so does the player");
}

/// **The subtle one.** A child deleted on its OWN, before the parent, must stay
/// deleted when the parent is restored. Restoring a club must not resurrect a
/// team someone deliberately deleted last week — which is exactly what a naive
/// "restore all children" would do. The shared cascade timestamp is what
/// distinguishes them.
#[tokio::test]
async fn restore_does_not_resurrect_an_independently_deleted_child() {
    let _g = lock().lock().await;
    boot().await;
    let club_id = seed("independent-club").await;

    // A second team, deleted on its own terms first.
    let pool = umbral::db::pool();
    let row = sqlx::query("INSERT INTO team (club, name) VALUES (?, 'reserves') RETURNING id")
        .bind(club_id)
        .fetch_one(&pool)
        .await
        .expect("team2");
    let team2: i64 = sqlx::Row::get(&row, 0);

    Team::objects()
        .filter(team::ID.eq(team2))
        .delete()
        .await
        .expect("delete reserves on its own");
    let after_independent = live("team").await;

    // Now the club goes, cascading to the remaining live team...
    Club::objects()
        .filter(club::ID.eq(club_id))
        .delete()
        .await
        .expect("soft delete club");
    assert_eq!(
        live("team").await,
        after_independent - 1,
        "cascade took the live one"
    );

    // ...and comes back.
    umbral::orm::DynQuerySet::for_meta(&umbral::migrate::ModelMeta::for_::<Club>())
        .filter_in_i64("id", &[club_id])
        .restore()
        .await
        .expect("restore");

    assert_eq!(
        live("team").await,
        after_independent,
        "restore brings back ONLY what the cascade took — the independently \
         deleted 'reserves' team must stay deleted",
    );
}
