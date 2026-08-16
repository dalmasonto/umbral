//! Identifying relations: a model whose PRIMARY KEY *is* a foreign key
//! (Prisma's `@id` on a relation field, a shared / identifying primary key).
//!
//! `Settings { @id user -> User }` recovers as
//! `#[umbral(primary_key)] pub user: ForeignKey<User>`. For that to compile
//! and behave, `ForeignKey<T>` has to satisfy `PrimaryKey` (Display +
//! Into<sea_query::Value> + the marker) and hash like its inner key. This
//! suite drives the real ORM path: create a child whose PK is the parent's
//! key, read it back, resolve the relation, and fetch it by that PK.

use tokio::sync::OnceCell;

use umbral::db;
use umbral::orm::ForeignKey;
use umbral::prelude::*;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]
#[umbral(table = "idrel_user")]
pub struct IdUser {
    #[umbral(primary_key)]
    pub slug: String,
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]
#[umbral(table = "idrel_settings")]
pub struct IdSettings {
    /// Identifying relation: the primary key IS the foreign key to `IdUser`
    /// (one row per user, sharing the user's key). No separate `id`.
    #[umbral(primary_key)]
    pub user: ForeignKey<IdUser>,
    pub theme: String,
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
            .model::<IdUser>()
            .model::<IdSettings>()
            .build()
            .expect("App::build");

        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
    })
    .await;
}

/// The PK-that-is-a-FK round-trips through the typed create + fetch path: the
/// child stores the parent's key, reads back the same key, and (because the
/// column is the PRIMARY KEY) a second insert with the same key is rejected.
#[tokio::test]
async fn identifying_relation_round_trips_and_enforces_pk() {
    boot().await;

    IdUser::objects()
        .create(IdUser {
            slug: "ada".to_string(),
            name: "Ada".to_string(),
        })
        .await
        .expect("create parent");

    IdSettings::objects()
        .create(IdSettings {
            user: ForeignKey::new("ada".to_string()),
            theme: "dark".to_string(),
        })
        .await
        .expect("create child whose PK is the FK");

    // Read the child back by its PK value — the PK column carries the key.
    let rows = IdSettings::objects()
        .filter(id_settings::USER.eq("ada"))
        .fetch()
        .await
        .expect("fetch settings by pk");
    assert_eq!(rows.len(), 1, "exactly one settings row for ada");
    assert_eq!(rows[0].user.id(), "ada", "the PK/FK is the parent's key");
    assert_eq!(rows[0].theme, "dark");

    // The column really is the PRIMARY KEY: a duplicate key must be rejected by
    // the DB, not silently inserted as a second row.
    let dup = IdSettings::objects()
        .create(IdSettings {
            user: ForeignKey::new("ada".to_string()),
            theme: "light".to_string(),
        })
        .await;
    assert!(
        dup.is_err(),
        "a second row with the same PK/FK must violate the primary key"
    );
}

/// `select_related` over the identifying FK hydrates the parent — the relation
/// still works when the FK doubles as the PK.
#[tokio::test]
async fn identifying_relation_resolves_the_parent() {
    boot().await;

    // Independent fixture keyed on "grace" so test order can't interfere.
    IdUser::objects()
        .create(IdUser {
            slug: "grace".to_string(),
            name: "Grace".to_string(),
        })
        .await
        .expect("create parent");
    IdSettings::objects()
        .create(IdSettings {
            user: ForeignKey::new("grace".to_string()),
            theme: "light".to_string(),
        })
        .await
        .expect("create child");

    let settings = IdSettings::objects()
        .select_related("user")
        .filter(id_settings::USER.eq("grace"))
        .fetch()
        .await
        .expect("fetch with select_related");

    let s = settings.first().expect("the grace settings row");
    let parent = s
        .user
        .resolved()
        .expect("parent hydrated via select_related");
    assert_eq!(parent.slug, "grace");
    assert_eq!(parent.name, "Grace");
}
