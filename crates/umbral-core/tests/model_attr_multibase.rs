//! gaps4 #89 — multi-base composition for `#[model(base = A, B, …)]`.
//!
//! The single-base attribute macro (see `model_attr_base.rs`) splices ONE
//! base's fields in flat. This exercises N bases: each base's fields are
//! spliced in, in declaration order, as REAL top-level fields — via the
//! continuation-passing `@compose` arms on the base companion macros (a naive
//! `A!{ B!{ … } }` can't work, because a macro's arguments aren't pre-expanded).

use chrono::{DateTime, Utc};
use tokio::sync::OnceCell;
use umbral::orm::Model;

// Base 1: the audit-timestamp base (carries the PK).
#[derive(
    Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow, umbral::orm::ModelBase,
)]
pub struct TimeStamped {
    #[umbral(primary_key)]
    pub id: i64,
    #[umbral(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbral(auto_now)]
    pub updated_at: DateTime<Utc>,
}

// Base 2: a soft-delete marker (no PK of its own).
#[derive(
    Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow, umbral::orm::ModelBase,
)]
pub struct SoftDelete {
    pub deleted_at: Option<DateTime<Utc>>,
}

// Base 3: a slug column (proves N > 2).
#[derive(
    Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow, umbral::orm::ModelBase,
)]
pub struct Slugged {
    #[umbral(string)]
    pub slug: String,
}

// TWO bases, one attribute. `id`/`created_at`/`updated_at` (TimeStamped) and
// `deleted_at` (SoftDelete) become flat, native fields ahead of `title`.
#[umbral::model(base = TimeStamped, SoftDelete)]
#[derive(
    Debug, Clone, Default, serde::Serialize, serde::Deserialize, sqlx::FromRow, umbral::orm::Model,
)]
#[umbral(table = "mmb_article")]
pub struct Article {
    #[umbral(string)]
    pub title: String,
}

// THREE bases.
#[umbral::model(base = TimeStamped, SoftDelete, Slugged)]
#[derive(
    Debug, Clone, Default, serde::Serialize, serde::Deserialize, sqlx::FromRow, umbral::orm::Model,
)]
#[umbral(table = "mmb_page")]
pub struct Page {
    #[umbral(string)]
    pub heading: String,
}

/// #89 (compile-time): every base's columns are REAL top-level fields, in
/// declaration order across the bases. If the composition were wrong, this
/// struct literal — naming `id`, `created_at`, `deleted_at`, `title` flat —
/// wouldn't compile.
#[test]
fn multiple_bases_splice_as_native_flat_fields() {
    let a = Article {
        id: 3,
        created_at: DateTime::<Utc>::from_timestamp(0, 0).unwrap(),
        updated_at: DateTime::<Utc>::from_timestamp(0, 0).unwrap(),
        deleted_at: None,
        title: "Hello".into(),
    };
    assert_eq!(a.id, 3);
    assert_eq!(a.title, "Hello");
    assert!(a.deleted_at.is_none());
}

/// The spliced columns land in `FIELDS` in base-declaration order, then the
/// model's own — and each keeps the attribute it carried on its base.
#[test]
fn base_columns_compose_in_declaration_order() {
    let names: Vec<&str> = <Article as Model>::FIELDS.iter().map(|f| f.name).collect();
    assert_eq!(
        names,
        vec!["id", "created_at", "updated_at", "deleted_at", "title"],
        "TimeStamped's columns, then SoftDelete's, then the model's own"
    );

    // Three bases compose the same way.
    let page_names: Vec<&str> = <Page as Model>::FIELDS.iter().map(|f| f.name).collect();
    assert_eq!(
        page_names,
        vec![
            "id",
            "created_at",
            "updated_at",
            "deleted_at",
            "slug",
            "heading"
        ]
    );

    // Attributes survive the compose for a column from the SECOND/THIRD base.
    let deleted = <Article as Model>::FIELDS
        .iter()
        .find(|f| f.name == "deleted_at")
        .unwrap();
    assert!(deleted.nullable, "SoftDelete.deleted_at stays nullable");

    let created = <Article as Model>::FIELDS
        .iter()
        .find(|f| f.name == "created_at")
        .unwrap();
    assert!(created.auto_now_add, "TimeStamped.auto_now_add survives");
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let pool = umbral_core::db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Article>()
            .model::<Page>()
            .build()
            .expect("App::build");
        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create schema from the composed FIELDS");
    })
    .await;
}

/// A create round-trip over a two-base model: the PK (base 1) autoincrements,
/// `auto_now_add` (base 1) stamps a real time, and `deleted_at` (base 2)
/// round-trips as NULL — all accessed as native flat fields.
#[tokio::test]
async fn create_round_trip_over_composed_bases() {
    boot().await;

    let row = Article::objects()
        .create(Article {
            title: "Composed".into(),
            ..Default::default()
        })
        .await
        .expect("create");

    assert!(row.id > 0, "base-1 PK autoincremented; got {}", row.id);
    assert!(
        row.created_at.timestamp() > 1_600_000_000,
        "base-1 auto_now_add stamped; got {}",
        row.created_at
    );
    assert!(
        row.deleted_at.is_none(),
        "base-2 column round-trips as NULL"
    );

    let fetched = Article::objects()
        .filter(article::ID.eq(row.id))
        .first()
        .await
        .expect("query")
        .expect("row present");
    assert_eq!(fetched.title, "Composed");
    assert_eq!(fetched.created_at.timestamp(), row.created_at.timestamp());
}

/// #67 across bases: typed column consts exist for columns from EVERY base
/// (and the model's own), with no `mixin_cols!` — because the compose makes
/// each base column the model's OWN field, the derive emits its const.
#[tokio::test]
async fn column_consts_from_every_base_compile_and_run() {
    boot().await;

    Article::objects()
        .create(Article {
            title: "Q".into(),
            ..Default::default()
        })
        .await
        .expect("create");

    let n = Article::objects()
        .filter(Article::ID.ge(0)) // base 1
        .filter(Article::CREATED_AT.gt(DateTime::<Utc>::from_timestamp(0, 0).unwrap())) // base 1
        .filter(Article::DELETED_AT.is_null()) // base 2 — const exists!
        .order_by(Article::TITLE.asc()) // own
        .count()
        .await
        .expect("query builds with consts from both bases");
    assert!(n >= 1);
}
