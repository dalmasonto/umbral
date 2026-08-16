//! Approach B: reusable model bases via `#[derive(ModelBase)]` + a
//! `#[umbral(flatten)]` embedded field. A base struct declares shared
//! columns (with the full `#[umbral(...)]` attribute set — `primary_key`,
//! `auto_now_add`, `auto_now`, …) once; any model embeds it as a nested
//! field and inherits those columns as if written inline.
//!
//! This is the Django abstract-base-model equivalent. The base's columns
//! must appear in the embedding model's `FIELDS` (in declaration order,
//! with every attribute preserved) so the whole ORM — migrations, the
//! SELECT list, inserts, auto-stamping — sees a flat schema.

use umbral::orm::{Model, SqlType};

// A reusable base carrying the PK plus audit timestamps — exactly the
// shape a Django `TimeStampedModel(models.Model)` abstract base produces.
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow, umbral::orm::Model)]
#[umbral(table = "mb_note")]
pub struct Note {
    #[umbral(flatten)]
    #[sqlx(flatten)]
    #[serde(flatten)]
    pub base: TimeStamped,
    pub title: String,
}

// A base bundling the primary key plus the soft-delete tombstone column.
// A model that embeds it AND marks itself `#[umbral(soft_delete)]` gets
// the framework's hide-instead-of-delete behavior for free.
#[derive(
    Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow, umbral::orm::ModelBase,
)]
pub struct SoftDeleteBase {
    #[umbral(primary_key)]
    pub id: i64,
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow, umbral::orm::Model)]
#[umbral(table = "mb_doc", soft_delete)]
pub struct Doc {
    #[umbral(flatten)]
    #[sqlx(flatten)]
    #[serde(flatten)]
    pub base: SoftDeleteBase,
    pub title: String,
}

// gaps5 #105: opt into typed column consts for the base-inherited fields,
// bound to `Note`. This makes `Note::ID` / `Note::CREATED_AT` /
// `Note::UPDATED_AT` usable in `filter` / `order_by` like own-field consts.
umbral::mixin_cols!(Note: TimeStamped);

/// The embedded base's columns land in `Note::FIELDS`, in declaration
/// order (base first, then the model's own fields), each carrying the
/// attribute it was declared with on the base.
#[test]
fn embedded_base_columns_appear_in_fields_in_declaration_order() {
    let names: Vec<&str> = <Note as Model>::FIELDS.iter().map(|f| f.name).collect();
    assert_eq!(
        names,
        vec!["id", "created_at", "updated_at", "title"],
        "base columns should be spliced in ahead of the model's own fields"
    );
}

/// The base's `#[umbral(primary_key)] id` is the model's primary key,
/// with the right SQL type — proving PK-in-base wiring works.
#[test]
fn primary_key_declared_on_the_base_is_the_models_primary_key() {
    let id = <Note as Model>::FIELDS
        .iter()
        .find(|f| f.name == "id")
        .expect("id column inherited from the base");
    assert!(id.primary_key, "base `id` should be the model PK");
    assert_eq!(id.ty, SqlType::BigInt);
    assert!(!id.nullable);
}

/// The `auto_now_add` / `auto_now` attributes declared on the base are
/// preserved on the embedding model's columns — so the write path still
/// stamps them.
#[test]
fn auto_now_attributes_survive_the_flatten() {
    let created = <Note as Model>::FIELDS
        .iter()
        .find(|f| f.name == "created_at")
        .expect("created_at inherited from base");
    assert!(created.auto_now_add, "created_at should keep auto_now_add");

    let updated = <Note as Model>::FIELDS
        .iter()
        .find(|f| f.name == "updated_at")
        .expect("updated_at inherited from base");
    assert!(updated.auto_now, "updated_at should keep auto_now");
}

/// `mixin_cols!` generates typed consts for the base-inherited columns
/// (`Note::ID`, `Note::CREATED_AT`, `Note::UPDATED_AT`), usable in
/// `filter` / `order_by` exactly like a model's own field consts — closing
/// the gaps5 #105 gap.
#[tokio::test]
async fn mixin_cols_generates_typed_base_column_consts() {
    boot().await;
    // If the consts didn't exist or had the wrong Col type, this wouldn't
    // compile; running it proves the built predicate/order are valid SQL.
    let _ = Note::objects()
        .filter(Note::ID.ge(0))
        .order_by(Note::CREATED_AT.desc())
        .order_by(Note::UPDATED_AT.asc())
        .count()
        .await
        .expect("query builds and runs with base-column consts");
}

// --------------------------------------------------------------------- //
// Live round-trip: create through the real ORM path, read back the       //
// nested base, and confirm the base's PK autoincrements + auto_now_add   //
// stamps — proving the flattened columns behave exactly like inline ones //
// across migrations, INSERT, and hydration.                              //
// --------------------------------------------------------------------- //

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
            .model::<Note>()
            .model::<Doc>()
            .build()
            .expect("App::build");
        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create schema from the flattened FIELDS");
    })
    .await;
}

/// A `Note` embedding `TimeStamped` inserts through `objects().create()`:
/// the base's `id` autoincrements (PK-in-base through the real INSERT),
/// `auto_now_add`/`auto_now` stamp real timestamps over the epoch
/// placeholder, and the nested `base` struct hydrates on read-back.
#[tokio::test]
async fn create_round_trip_stamps_base_and_autoincrements_base_pk() {
    boot().await;

    // Epoch placeholders: if the write path didn't stamp them, they'd
    // persist as 1970 and the assertions below would fail.
    let epoch = chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap();
    let new = Note {
        base: TimeStamped {
            id: 0, // autoincrement sentinel — the base carries the PK
            created_at: epoch,
            updated_at: epoch,
        },
        title: "flattened".into(),
    };

    let row = Note::objects().create(new).await.expect("create");

    assert!(
        row.base.id > 0,
        "base PK should autoincrement through the flattened INSERT; got {}",
        row.base.id
    );
    assert_eq!(row.title, "flattened");
    assert!(
        row.base.created_at.timestamp() > 1_600_000_000,
        "auto_now_add should have stamped a real `created_at`, not the epoch placeholder; got {}",
        row.base.created_at
    );
    assert!(
        row.base.updated_at.timestamp() > 1_600_000_000,
        "auto_now should have stamped a real `updated_at`; got {}",
        row.base.updated_at
    );

    // Read back through the ORM (filtering on the model's OWN column, for
    // which a typed const exists) to confirm the row persisted and the
    // nested base re-hydrates from the flat columns.
    let fetched = Note::objects()
        .filter(note::TITLE.eq("flattened"))
        .first()
        .await
        .expect("query")
        .expect("row present");
    assert_eq!(fetched.base.id, row.base.id, "base PK re-hydrates on read");
    assert_eq!(
        fetched.base.created_at.timestamp(),
        row.base.created_at.timestamp()
    );
}

/// A model marked `#[umbral(soft_delete)]` whose `deleted_at` tombstone
/// column is inherited from an embedded base behaves exactly like an
/// inline soft-delete model: `delete()` hides the row (default queries
/// skip it) rather than removing it, and `.with_deleted()` still sees it.
#[tokio::test]
async fn soft_delete_column_inherited_from_a_base_hides_rows() {
    boot().await;

    let doc = Doc::objects()
        .create(Doc {
            base: SoftDeleteBase {
                id: 0,
                deleted_at: None,
            },
            title: "gone-soon".into(),
        })
        .await
        .expect("create");
    assert!(doc.base.id > 0);

    // Soft-delete: UPDATE ... SET deleted_at = now(), not a hard DELETE.
    let removed = Doc::objects()
        .filter(doc::TITLE.eq("gone-soon"))
        .delete()
        .await
        .expect("soft delete");
    assert_eq!(removed, 1);

    // Default queries skip soft-deleted rows (auto WHERE deleted_at IS NULL,
    // on the base-inherited column).
    let visible = Doc::objects()
        .filter(doc::TITLE.eq("gone-soon"))
        .count()
        .await
        .expect("count");
    assert_eq!(visible, 0, "soft-deleted row hidden from default queries");

    // `.with_deleted()` opts back in and finds the tombstoned row.
    let including = Doc::objects()
        .filter(doc::TITLE.eq("gone-soon"))
        .with_deleted()
        .count()
        .await
        .expect("count with deleted");
    assert_eq!(including, 1, "row still present, just tombstoned");
}
