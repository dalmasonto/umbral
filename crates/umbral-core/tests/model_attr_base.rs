//! `#[model(base = …)]` — the attribute-macro form of base embedding
//! (gaps4 #62/#64/#67). Unlike the derive-based `#[umbral(flatten)]`
//! mechanism (see `model_base.rs`), this inlines the base's fields as REAL,
//! top-level fields, so:
//!
//! - #62: inherited fields are accessed NATIVELY — `country.id`,
//!   `country.created_at`, NOT `country.base.id`.
//! - #64: the developer writes ONE attribute (`#[model(base = …)]`) — no
//!   nested `base` field, no `#[serde(flatten)]` / `#[sqlx(flatten)]` trio.
//! - #67: the base's typed column consts (`Country::CREATED_AT`) exist with
//!   no hand-written `mixin_cols!` line.
//!
//! The two mechanisms coexist: `model_base.rs` keeps exercising the old
//! `#[umbral(flatten)]` path, and its assertions must stay green.

use umbral::orm::{Model, SqlType};

// A reusable audit-timestamp base — the exact shape a Django
// `TimeStampedModel` abstract base produces. Note: NO hand-written
// `Default`/`new()` (the derive auto-emits them, gaps4 #63b).
#[derive(
    Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow, umbral::orm::ModelBase,
)]
pub struct TimeStamped {
    #[umbral(primary_key)]
    pub id: i64,
    #[umbral(auto_now_add)]
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[umbral(auto_now)]
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

// The whole point: ONE attribute, written ABOVE the derive line. No nested
// `base` field, no `#[serde(flatten)]`/`#[sqlx(flatten)]`, no `mixin_cols!`.
// `id` / `created_at` / `updated_at` are spliced in as flat fields by the
// `__umbral_base_fields_TimeStamped!` companion macro before the derives run.
#[umbral::model(base = TimeStamped)]
#[derive(
    Debug, Clone, Default, serde::Serialize, serde::Deserialize, sqlx::FromRow, umbral::orm::Model,
)]
#[umbral(table = "mab_country")]
pub struct Country {
    pub name: String,
    pub slug: String,
}

/// #62 + #64 (compile-time): the inherited base columns are REAL top-level
/// fields — `c.id`, `c.created_at` compile with no `.base.` indirection, and
/// there is no nested `base` field anywhere. If the macro emitted a nested
/// struct instead, this function wouldn't compile.
#[test]
fn inherited_base_fields_are_native_top_level_fields() {
    let c = Country {
        id: 7,
        created_at: chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap(),
        updated_at: chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap(),
        name: "Kenya".into(),
        slug: "kenya".into(),
    };
    // Native access — the #62 fix. `c.base.id` would NOT compile.
    assert_eq!(c.id, 7);
    assert_eq!(c.name, "Kenya");
    assert_eq!(c.slug, "kenya");
    assert_eq!(c.created_at.timestamp(), 0);
}

/// The spliced base columns land in `FIELDS` ahead of the model's own,
/// each keeping the attribute it was declared with on the base — schema
/// identical to the old `#[umbral(flatten)]` mechanism (proves the
/// "coexist, no migration" claim).
#[test]
fn base_columns_appear_first_in_fields_with_attrs_preserved() {
    let names: Vec<&str> = <Country as Model>::FIELDS.iter().map(|f| f.name).collect();
    assert_eq!(
        names,
        vec!["id", "created_at", "updated_at", "name", "slug"]
    );

    let id = <Country as Model>::FIELDS
        .iter()
        .find(|f| f.name == "id")
        .unwrap();
    assert!(id.primary_key, "base `id` is the model PK");
    assert_eq!(id.ty, SqlType::BigInt);
    assert!(!id.nullable);

    let created = <Country as Model>::FIELDS
        .iter()
        .find(|f| f.name == "created_at")
        .unwrap();
    assert!(created.auto_now_add, "auto_now_add survives the splice");

    let updated = <Country as Model>::FIELDS
        .iter()
        .find(|f| f.name == "updated_at")
        .unwrap();
    assert!(updated.auto_now, "auto_now survives the splice");
}

use tokio::sync::OnceCell;

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
            .model::<Country>()
            .build()
            .expect("App::build");
        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create schema from the spliced FIELDS");
    })
    .await;
}

/// #62 round-trip: `create()` through the real ORM autoincrements the
/// base-declared PK and stamps `auto_now_add`/`auto_now` over the epoch
/// placeholder, and read-back hydrates the flat inherited columns — all
/// accessed natively (`row.id`, `row.created_at`).
#[tokio::test]
async fn create_round_trip_over_native_base_fields() {
    boot().await;

    let new = Country {
        id: 0, // autoincrement sentinel
        created_at: chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap(),
        updated_at: chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap(),
        name: "Uganda".into(),
        slug: "uganda".into(),
    };

    let row = Country::objects().create(new).await.expect("create");
    assert!(
        row.id > 0,
        "base PK autoincremented natively; got {}",
        row.id
    );
    assert_eq!(row.name, "Uganda");
    assert!(
        row.created_at.timestamp() > 1_600_000_000,
        "auto_now_add stamped a real created_at over the epoch; got {}",
        row.created_at
    );
    assert!(row.updated_at.timestamp() > 1_600_000_000);

    let fetched = Country::objects()
        .filter(country::SLUG.eq("uganda"))
        .first()
        .await
        .expect("query")
        .expect("row present");
    assert_eq!(fetched.id, row.id, "base PK re-hydrates natively on read");
    assert_eq!(fetched.created_at.timestamp(), row.created_at.timestamp());
}

/// #67: typed column consts for the INHERITED base columns exist with NO
/// hand-written `mixin_cols!` line in this file — because the splice makes
/// `id`/`created_at`/`updated_at` the model's OWN fields, `#[derive(Model)]`
/// auto-emits `Country::CREATED_AT` / `Country::ID` exactly like `Country::SLUG`.
/// If the splice were missing, these references wouldn't compile.
#[tokio::test]
async fn base_column_consts_auto_generated_no_manual_mixin() {
    boot().await;

    Country::objects()
        .create(Country {
            id: 0,
            created_at: chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap(),
            updated_at: chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap(),
            name: "Tanzania".into(),
            slug: "tanzania".into(),
        })
        .await
        .expect("create");

    // `Country::CREATED_AT` (inherited) and `Country::SLUG` (own) both work
    // in filter/order_by — the whole #67 promise ("the base's columns are
    // mine now") with zero extra lines.
    let n = Country::objects()
        .filter(Country::ID.ge(0))
        .filter(
            Country::CREATED_AT.gt(chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap()),
        )
        .order_by(Country::CREATED_AT.desc())
        .order_by(Country::UPDATED_AT.asc())
        .count()
        .await
        .expect("query builds and runs with auto-generated base-column consts");
    assert!(n >= 1, "the row we inserted has created_at > epoch");
}

/// #64 + #63b: construction collapses to `..Default::default()` — the model
/// derives `Default` (every field, base-derived and own, is `Default`), so a
/// caller supplies only the meaningful fields. The auto-managed PK/timestamps
/// are overwritten by the real INSERT path, so the read-back row carries a
/// real assigned PK and a real (not epoch) timestamp.
#[tokio::test]
async fn construction_collapses_to_default() {
    boot().await;

    let new = Country {
        name: "Rwanda".into(),
        slug: "rwanda".into(),
        ..Default::default()
    };
    let row = Country::objects().create(new).await.expect("create");

    assert!(row.id > 0, "PK autoincremented over the Default 0 sentinel");
    assert!(
        row.created_at.timestamp() > 1_600_000_000,
        "auto_now_add stamped over the Default epoch; got {}",
        row.created_at
    );

    let fetched = Country::objects()
        .filter(country::SLUG.eq("rwanda"))
        .first()
        .await
        .expect("query")
        .expect("row present");
    assert_eq!(fetched.id, row.id);
}
