// Test models below are private-by-default but the derive emits
// `pub const` column constants referencing them; that trips the
// `private_interfaces` lint at every const. They're also data carriers
// whose .value field isn't read by the static FIELDS assertions. Both
// patterns are intentional in test code, so the lints are silenced at
// the file level rather than crowding every struct with attributes.
#![allow(dead_code, private_interfaces)]

//! End-to-end coverage for the M3 type-catalogue refresh: every Rust field
//! type the derive now recognises should land in the right `SqlType` slot,
//! `Option<T>` of any of those should set `nullable: true`, the right
//! sibling column type should be emitted (so e.g. `Option<String>` exposes
//! `is_null`), and the primary-key detection should pick `i32`, `i64`, or
//! `uuid::Uuid` straight off the `id` field's type.
//!
//! Integration-tests choice. Each file under `tests/` compiles to its own
//! binary, so the process-wide `OnceLock`s in `umbral_core::db` start empty
//! for every run and can't be polluted by a sibling test that already
//! called `App::build()`. That isolation lets one file declare a small zoo
//! of models, derive `Model` on each at module scope, and exercise the
//! whole expanded catalogue end-to-end (including a real sqlx roundtrip
//! for the Uuid path) without stepping on `model_trait.rs` or `builder.rs`.
//!
//! Note for future readers. The models below are declared at module scope,
//! not inside `#[test]` fns, because the `#[derive(Model)]` expansion
//! emits a sibling column module named after the snake_case struct ident
//! and references `super::<Struct>`. Putting the struct inside a fn would
//! park that column module inside the fn body, where `super::` resolves
//! to this file's module and breaks the link.

use umbral::orm::Model;
use umbral_core::orm::{FieldSpec, SqlType};

// --------------------------------------------------------------------- //
// Int classification: small ints (i8, i16, u8) -> SmallInt; 32-bit ints  //
// (i32, u16) -> Integer; 64-bit ints (i64, u32) -> BigInt. Each model    //
// carries its own `id: i64` so the M3 primary-key requirement is met,    //
// plus one `value` field of the int type under test in slot [1].         //
// --------------------------------------------------------------------- //

#[derive(Debug, sqlx::FromRow, umbral::orm::Model)]
struct SmallI8 {
    id: i64,
    value: i8,
}

#[derive(Debug, sqlx::FromRow, umbral::orm::Model)]
struct SmallI16 {
    id: i64,
    value: i16,
}

#[derive(Debug, sqlx::FromRow, umbral::orm::Model)]
struct SmallU8 {
    id: i64,
    value: u8,
}

#[derive(Debug, sqlx::FromRow, umbral::orm::Model)]
struct IntI32 {
    id: i64,
    value: i32,
}

#[derive(Debug, sqlx::FromRow, umbral::orm::Model)]
struct IntU16 {
    id: i64,
    value: u16,
}

#[derive(Debug, sqlx::FromRow, umbral::orm::Model)]
struct BigI64 {
    id: i64,
    value: i64,
}

#[derive(Debug, sqlx::FromRow, umbral::orm::Model)]
struct BigU32 {
    id: i64,
    value: u32,
}

/// Each int width should classify into the right `SqlType` bucket: small
/// ints into `SmallInt`, 32-bit signed and 16-bit unsigned into `Integer`,
/// 64-bit signed and 32-bit unsigned into `BigInt`. The `value` column is
/// always slot `[1]` because `id` occupies `[0]`.
#[test]
fn int_sizes_classify_correctly() {
    assert_eq!(
        SmallI8::FIELDS[1].ty,
        SqlType::SmallInt,
        "i8 must map to SqlType::SmallInt",
    );
    assert_eq!(
        SmallI16::FIELDS[1].ty,
        SqlType::SmallInt,
        "i16 must map to SqlType::SmallInt",
    );
    assert_eq!(
        SmallU8::FIELDS[1].ty,
        SqlType::SmallInt,
        "u8 must map to SqlType::SmallInt",
    );
    assert_eq!(
        IntI32::FIELDS[1].ty,
        SqlType::Integer,
        "i32 must map to SqlType::Integer",
    );
    assert_eq!(
        IntU16::FIELDS[1].ty,
        SqlType::Integer,
        "u16 must map to SqlType::Integer",
    );
    assert_eq!(
        BigI64::FIELDS[1].ty,
        SqlType::BigInt,
        "i64 must map to SqlType::BigInt",
    );
    assert_eq!(
        BigU32::FIELDS[1].ty,
        SqlType::BigInt,
        "u32 must map to SqlType::BigInt",
    );
}

// --------------------------------------------------------------------- //
// Float classification: f32 -> Real, f64 -> Double. The column type is   //
// `F64Col` for both because f32 fits losslessly into f64, but the        //
// `SqlType` tag keeps the precision distinction the migration engine    //
// will render at M5.                                                     //
// --------------------------------------------------------------------- //

#[derive(Debug, sqlx::FromRow, umbral::orm::Model)]
struct FloatRow {
    id: i64,
    ratio: f32,
    weight: f64,
}

/// f32 and f64 should classify into Real and Double respectively, keeping
/// the precision distinction visible at the `SqlType` tag level even
/// though both share the `F64Col` column type at runtime.
#[test]
fn float_types_classify_correctly() {
    assert_eq!(
        FloatRow::FIELDS[1].ty,
        SqlType::Real,
        "f32 must map to SqlType::Real so the migration engine renders REAL/FLOAT4",
    );
    assert_eq!(
        FloatRow::FIELDS[2].ty,
        SqlType::Double,
        "f64 must map to SqlType::Double so the migration engine renders DOUBLE/FLOAT8",
    );
}

// --------------------------------------------------------------------- //
// Bool classification: bool -> Boolean. One column, one assertion.       //
// --------------------------------------------------------------------- //

#[derive(Debug, sqlx::FromRow, umbral::orm::Model)]
struct FlagRow {
    id: i64,
    active: bool,
}

/// A plain `bool` field should land at `SqlType::Boolean`.
#[test]
fn bool_type_classifies_to_boolean() {
    assert_eq!(
        FlagRow::FIELDS[1].ty,
        SqlType::Boolean,
        "bool must map to SqlType::Boolean",
    );
}

// --------------------------------------------------------------------- //
// chrono date/time classification: NaiveDate -> Date, NaiveTime -> Time. //
// The Timestamptz path is already covered by `model_trait.rs` through    //
// `Post::published_at`, so it isn't re-tested here.                      //
// --------------------------------------------------------------------- //

#[derive(Debug, sqlx::FromRow, umbral::orm::Model)]
struct CalendarRow {
    id: i64,
    day: chrono::NaiveDate,
    moment: chrono::NaiveTime,
}

/// `chrono::NaiveDate` should classify into `SqlType::Date` and
/// `chrono::NaiveTime` into `SqlType::Time`, keeping date and time as
/// distinct column types.
#[test]
fn chrono_date_and_time_classify_correctly() {
    assert_eq!(
        CalendarRow::FIELDS[1].ty,
        SqlType::Date,
        "chrono::NaiveDate must map to SqlType::Date",
    );
    assert_eq!(
        CalendarRow::FIELDS[2].ty,
        SqlType::Time,
        "chrono::NaiveTime must map to SqlType::Time",
    );
}

// --------------------------------------------------------------------- //
// Uuid classification: a non-PK uuid field should land at SqlType::Uuid. //
// The PK-as-uuid case is covered separately below to keep the failure    //
// modes distinct: this test fails if the classifier misses Uuid, the     //
// other one fails if `PrimaryKey for Uuid` is wired wrong.               //
// --------------------------------------------------------------------- //

#[derive(Debug, sqlx::FromRow, umbral::orm::Model)]
struct TaggedRow {
    id: i64,
    tag: uuid::Uuid,
}

/// A non-PK `uuid::Uuid` field should classify into `SqlType::Uuid`.
#[test]
fn uuid_classifies_to_uuid() {
    assert_eq!(
        TaggedRow::FIELDS[1].ty,
        SqlType::Uuid,
        "uuid::Uuid must map to SqlType::Uuid",
    );
}

// --------------------------------------------------------------------- //
// Nullable variants: `Option<T>` of any catalogue type sets             //
// `nullable: true` on the field spec. The `SqlType` underneath is the   //
// inner type's tag (the nullable-ness lives on the FieldSpec, not on    //
// SqlType itself) so the migration engine renders the right column type //
// with a `NULL` modifier rather than inventing nullable SqlType         //
// variants.                                                              //
// --------------------------------------------------------------------- //

#[derive(Debug, sqlx::FromRow, umbral::orm::Model)]
struct NullableRow {
    id: i64,
    name: Option<String>,
    count: Option<i64>,
    tag: Option<uuid::Uuid>,
}

/// `Option<T>` for any catalogue type should flip the FieldSpec's
/// `nullable` flag, and the underlying SqlType should still reflect the
/// inner Rust type (Text for Option<String>, BigInt for Option<i64>,
/// Uuid for Option<uuid::Uuid>).
#[test]
fn nullable_variants_set_nullable_true() {
    let by_name = |name: &str| -> &FieldSpec {
        NullableRow::FIELDS
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("NullableRow::FIELDS should contain '{name}'"))
    };

    let name = by_name("name");
    assert!(
        name.nullable,
        "Option<String> must set nullable: true on the FieldSpec",
    );
    assert_eq!(
        name.ty,
        SqlType::Text,
        "Option<String> should keep SqlType::Text under the nullable wrapper",
    );

    let count = by_name("count");
    assert!(
        count.nullable,
        "Option<i64> must set nullable: true on the FieldSpec",
    );
    assert_eq!(
        count.ty,
        SqlType::BigInt,
        "Option<i64> should keep SqlType::BigInt under the nullable wrapper",
    );

    let tag = by_name("tag");
    assert!(
        tag.nullable,
        "Option<uuid::Uuid> must set nullable: true on the FieldSpec",
    );
    assert_eq!(
        tag.ty,
        SqlType::Uuid,
        "Option<uuid::Uuid> should keep SqlType::Uuid under the nullable wrapper",
    );
}

// --------------------------------------------------------------------- //
// Nullable column-type selection: `Option<String>` should emit a sibling //
// `NullableStrCol` constant, which is the only String column variant     //
// that exposes `is_null`. Compiling `BODY.is_null()` is the type-level   //
// check; if the derive emitted a plain `StrCol` instead, this test would //
// fail to compile rather than at runtime.                                //
// --------------------------------------------------------------------- //

#[derive(Debug, sqlx::FromRow, umbral::orm::Model)]
struct OptStringRow {
    id: i64,
    body: Option<String>,
}

/// `Option<String>` should drive the derive to emit a `NullableStrCol`,
/// which exposes `is_null` / `is_not_null` — the methods a plain `StrCol`
/// doesn't have. The body of this test compiles iff the column type is
/// the nullable one; if subagent B's derive emits `StrCol` by mistake,
/// `opt_string_row::BODY.is_null()` fails to resolve and the whole file
/// stops compiling. That's the failure mode we want.
#[test]
fn string_nullable_picks_nullable_str_col() {
    let _predicate = opt_string_row::BODY.is_null();
    let _predicate2 = opt_string_row::BODY.is_not_null();
}

// --------------------------------------------------------------------- //
// Primary key detection. The derive picks `i32`, `i64`, or `uuid::Uuid`  //
// from the `id` field's type. The `let _: T = ...primary_key()` pattern  //
// is a real type-level check: if the derive emits the wrong               //
// `type PrimaryKey`, the binding fails to typecheck and this file does   //
// not compile. That's preferred over a runtime assertion because it       //
// catches the bug at the earliest possible moment.                        //
// --------------------------------------------------------------------- //

#[derive(Debug, sqlx::FromRow, umbral::orm::Model)]
struct PkI32 {
    id: i32,
    name: String,
}

/// `id: i32` should drive `<PkI32 as Model>::PrimaryKey = i32`. The
/// `let _: i32 = ...` binding only typechecks if the associated type
/// really is `i32`; a wrong choice (i64, Uuid, …) is a compile error.
#[test]
fn primary_key_i32_works() {
    assert_eq!(PkI32::TABLE, "pk_i32");
    let row = PkI32 {
        id: 1,
        name: String::new(),
    };
    let _: i32 = row.primary_key();
}

#[derive(Debug, sqlx::FromRow, umbral::orm::Model)]
struct PkUuid {
    id: uuid::Uuid,
    label: String,
}

/// `id: uuid::Uuid` should drive `<PkUuid as Model>::PrimaryKey =
/// uuid::Uuid`. The `let _: uuid::Uuid = ...` binding only typechecks if
/// the derive picked the Uuid path; an `i64` fallback is a compile error.
/// `Uuid::nil()` is used here to avoid any feature dependence on uuid's
/// random generators — the value isn't compared, only its type is.
#[test]
fn primary_key_uuid_works() {
    assert_eq!(PkUuid::TABLE, "pk_uuid");
    let row = PkUuid {
        id: uuid::Uuid::nil(),
        label: String::new(),
    };
    let _: uuid::Uuid = row.primary_key();
}

// --------------------------------------------------------------------- //
// End-to-end async DB roundtrip for the Uuid PK path. The test creates   //
// an in-memory SQLite pool, declares a table with a TEXT id, inserts     //
// one row with a known `Uuid::now_v7()`, and reads it back through       //
// `sqlx::query_as::<_, T>`. The roundtrip exercises three things at      //
// once: (a) the derive's `#[derive(sqlx::FromRow)]` carry for the Uuid   //
// column, (b) sqlx's `uuid` feature wiring through the workspace, and    //
// (c) the SqliteRow decoder for uuid via the TEXT column type. v7 is     //
// used for variety since the workspace already enables it.                //
// --------------------------------------------------------------------- //

#[derive(Debug, sqlx::FromRow, umbral::orm::Model)]
struct UuidRoundtrip {
    id: uuid::Uuid,
    label: String,
}

/// Insert one row with a known v7 Uuid, read it back through `query_as`,
/// confirm both the Uuid and the label survived the roundtrip. Proves
/// the FromRow + sqlx `uuid` feature wiring is correct end-to-end and
/// closes the loop on the Uuid PK path the static tests above type-check.
#[tokio::test]
async fn uuid_primary_key_roundtrips_through_sqlx() {
    let pool = umbral_core::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite should always connect");

    // SQLite has no native UUID type; uuid round-trips via TEXT. That is
    // the same encoding the M4 SQLite backend will produce when it
    // renders `SqlType::Uuid` for a SQLite migration.
    sqlx::query(
        "CREATE TABLE uuid_roundtrip (\
             id TEXT PRIMARY KEY NOT NULL,\
             label TEXT NOT NULL\
         )",
    )
    .execute(&pool)
    .await
    .expect("CREATE TABLE uuid_roundtrip should succeed on a fresh in-memory database");

    let seed = uuid::Uuid::now_v7();

    sqlx::query("INSERT INTO uuid_roundtrip (id, label) VALUES (?, ?)")
        .bind(seed)
        .bind("greetings")
        .execute(&pool)
        .await
        .expect("INSERT into uuid_roundtrip should succeed");

    let row: UuidRoundtrip =
        sqlx::query_as::<_, UuidRoundtrip>("SELECT id, label FROM uuid_roundtrip WHERE id = ?")
            .bind(seed)
            .fetch_one(&pool)
            .await
            .expect(
                "query_as::<_, UuidRoundtrip> should decode the row via the FromRow supertrait",
            );

    assert_eq!(
        row.id, seed,
        "the Uuid should survive the TEXT-encoded roundtrip unchanged",
    );
    assert_eq!(row.label, "greetings");
    let _: uuid::Uuid = row.primary_key();
}

// --------------------------------------------------------------------- //
// M3.1 — `#[umbral(table = "...")]` overrides the default snake_case-of- //
// struct-name table name. Plugin authors hit this when they want a     //
// prefix the snake_case round-trip can't produce (e.g. struct `User`   //
// with table `auth_user`, or the inspectdb edge case where an existing //
// SQL table uses non-conventional casing).                             //
// --------------------------------------------------------------------- //

#[derive(Debug, sqlx::FromRow, umbral::orm::Model)]
#[umbral(table = "auth_user_override")]
struct UserWithCustomTable {
    id: i64,
    username: String,
}

#[test]
fn umbral_table_attribute_overrides_the_default_table_name() {
    assert_eq!(
        UserWithCustomTable::TABLE,
        "auth_user_override",
        "the #[umbral(table = \"...\")] attribute should override the default snake_case",
    );
    // The default would have been `user_with_custom_table`; the
    // override wins.
    assert_ne!(UserWithCustomTable::TABLE, "user_with_custom_table");
    // The Model::NAME stays the Rust struct name regardless.
    assert_eq!(UserWithCustomTable::NAME, "UserWithCustomTable");
}

// --------------------------------------------------------------------- //
// FEATURES.md #1 — primary-key catalogue extension. The `PrimaryKey`    //
// trait ships impls for every integer width plus `String`. The derive  //
// no longer hardcodes a per-type check; any type implementing the      //
// trait works as a model's id.                                         //
// --------------------------------------------------------------------- //

#[derive(Debug, sqlx::FromRow, umbral::orm::Model)]
struct PkU32 {
    id: u32,
    name: String,
}

/// `id: u32` should drive `<PkU32 as Model>::PrimaryKey = u32`. Before
/// FEATURES.md #1 closed, this would have failed at derive time with
/// "umbral M3 supports id types i32, i64, or uuid::Uuid".
#[test]
fn primary_key_u32_works() {
    let row = PkU32 {
        id: 42,
        name: String::new(),
    };
    let pk: u32 = row.primary_key();
    assert_eq!(pk, 42);
}

#[derive(Debug, Clone, sqlx::FromRow, umbral::orm::Model)]
struct PkString {
    id: String,
    title: String,
}

/// `id: String` is the slug-style key pattern (a string PK).
/// The trait bound is now `Clone`, not `Copy`, so non-Copy types
/// like `String` work the same way as the integer types.
#[test]
fn primary_key_string_works() {
    let row = PkString {
        id: "hello-world".to_string(),
        title: String::new(),
    };
    let pk: String = row.primary_key();
    assert_eq!(pk, "hello-world");
}

// User-defined newtype PK. Demonstrates the public extension path:
// `From<MyId> for sea_query::Value` plus `impl PrimaryKey for MyId {}`
// are the two lines a user crate writes — `Into<sea_query::Value>` is
// the bound the M2M junction-table CRUD path needs to bind the PK on
// both backends without per-type adapters.
#[derive(Debug, Clone, PartialEq, Eq)]
struct UserScopedId(u64);

impl From<UserScopedId> for sea_query::Value {
    fn from(id: UserScopedId) -> Self {
        id.0.into()
    }
}

// `Display` is part of the `PrimaryKey` trait surface — the framework
// uses it to stringify a typed PK for session storage and for the
// uniform `Identity::user_id` field on the REST plugin's Identity.
// Custom PK types must provide it; the default for tuple-struct
// wrappers is to forward to the inner type.
impl std::fmt::Display for UserScopedId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl umbral::orm::PrimaryKey for UserScopedId {}

// NOTE: the model itself can't currently use UserScopedId as its `id`
// field because the M3 derive's field-type classifier doesn't know
// about user-defined types — `classify_field_type` would return
// `Unsupported`. The PK trait extension shipped here is the first
// half; the field-type extension is a follow-on (FEATURES.md medium
// #1, second pass). This test pins the trait surface so the impl side
// compiles and a future model author can wire the full path.
#[test]
fn user_defined_primary_key_type_compiles() {
    let id = UserScopedId(7);
    // The bound `T: PrimaryKey` is satisfied.
    fn accept<T: umbral::orm::PrimaryKey>(_: T) {}
    accept(id.clone());
    assert_eq!(id, UserScopedId(7));
}
