//! End-to-end coverage for the M2 `Model` trait surface as implemented by `Post`.
//!
//! The trait is mostly static metadata plus a tiny primary-key getter, so the
//! bulk of these checks are sync `#[test]`s reading `TABLE`, `FIELDS`, and the
//! `PrimaryKey` associated type. One `#[tokio::test]` at the bottom exercises
//! the `for<'r> FromRow<'r, SqliteRow>` supertrait against a real in-memory
//! SQLite pool, which is the M2 invariant QuerySet terminals rely on to
//! blanket `T: Model` and call `sqlx::query_as_with::<_, T, _>`.
//!
//! Living under `tests/` rather than alongside `src/orm/model.rs` matters for
//! the async case: each file in `tests/` compiles to its own test binary, so
//! the process-wide `OnceLock`s in `umbral_core::db` start empty for every run
//! and can't be polluted by a sibling test that already called `App::build()`.
//! It also keeps the sync trait checks and the async DB check in the same
//! file without splitting them across compilation units.

use umbral_core::db;
use umbral_core::orm::{FieldSpec, Model, Post, SqlType};

/// The `TABLE` constant should be the lowercase struct name. M3's derive will
/// default to `snake_case(<StructName>)`; the hand-written M2 impl pins the
/// `Post` -> `"post"` mapping that the derive has to reproduce.
#[test]
fn post_table_constant_matches_struct_name() {
    assert_eq!(Post::TABLE, "post");
}

/// `FIELDS` lists every column in struct declaration order. That order is
/// the contract M5's migration engine relies on for snapshot diffing, so
/// drift here would silently break autodetection later.
#[test]
fn post_has_four_fields_in_declaration_order() {
    assert_eq!(
        Post::FIELDS.len(),
        4,
        "Post has exactly four columns: id, title, body, published_at",
    );

    let names: Vec<&str> = Post::FIELDS.iter().map(|f| f.name).collect();
    assert_eq!(
        names,
        vec!["id", "title", "body", "published_at"],
        "FIELDS must mirror struct declaration order",
    );
}

/// Exactly one field carries `primary_key: true`, and it's `id`. Models with
/// no PK or multiple PKs are spec violations at M2; this guards the canonical
/// single-column shape.
#[test]
fn post_primary_key_field_is_id() {
    let pks: Vec<&FieldSpec> = Post::FIELDS.iter().filter(|f| f.primary_key).collect();

    assert_eq!(
        pks.len(),
        1,
        "exactly one field should be marked as the primary key",
    );
    assert_eq!(pks[0].name, "id", "the primary key column should be 'id'");
}

/// Exactly one field is `nullable: true`, and it's `published_at`. This
/// mirrors the `Option<DateTime<Utc>>` field on the struct and pins the
/// `04-orm-model-and-fields.md` invariant that NULL is reachable only via
/// `Option<T>`.
#[test]
fn post_published_at_is_the_only_nullable_field() {
    let nullable: Vec<&FieldSpec> = Post::FIELDS.iter().filter(|f| f.nullable).collect();

    assert_eq!(
        nullable.len(),
        1,
        "exactly one field should be nullable (matches the lone Option<T> on Post)",
    );
    assert_eq!(
        nullable[0].name, "published_at",
        "the nullable field should be 'published_at'",
    );
}

/// Each field's `SqlType` tag should match its Rust type: `id` -> BigInt,
/// `title` / `body` -> Text, `published_at` -> Timestamptz. The dialect
/// rendering is the backend's job; this enum is the abstract classification
/// the system check (M4) and migration engine (M5) reason about.
#[test]
fn post_field_types_are_correct() {
    let by_name = |name: &str| {
        Post::FIELDS
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("Post::FIELDS should contain '{name}'"))
    };

    assert_eq!(by_name("id").ty, SqlType::BigInt);
    assert_eq!(by_name("title").ty, SqlType::Text);
    assert_eq!(by_name("body").ty, SqlType::Text);
    assert_eq!(by_name("published_at").ty, SqlType::Timestamptz);
}

/// Every field on `Post` declares an empty `supported_backends` slice, which
/// per the trait doc means "all backends." No Postgres-only field types
/// (arrays, JSONB, etc.) are in play at M2, so the system check has nothing
/// to reject.
#[test]
fn post_supported_backends_is_empty_for_every_field() {
    for field in Post::FIELDS {
        assert!(
            field.supported_backends.is_empty(),
            "field '{}' should declare no backend restrictions at M2, got {:?}",
            field.name,
            field.supported_backends,
        );
    }
}

/// `primary_key()` should return the value of the `id` field on the instance.
/// Exercises the only fn on the trait, not just its constants.
#[test]
fn primary_key_getter_returns_id_field() {
    let post = Post {
        id: 42,
        title: "any title".to_string(),
        body: "any body".to_string(),
        published_at: None,
    };

    assert_eq!(
        post.primary_key(),
        42,
        "primary_key() should hand back the id field verbatim",
    );
}

/// `<Post as Model>::PrimaryKey` is `i64` at M2 (UUID lands later). The
/// assignment below only compiles if the associated type really is `i64`, so
/// this is a type-level check the compiler enforces; the runtime assert is
/// belt-and-braces.
#[test]
fn primary_key_type_is_i64() {
    let post = Post {
        id: 7,
        title: String::new(),
        body: String::new(),
        published_at: None,
    };

    let pk: <Post as Model>::PrimaryKey = post.primary_key();
    let pk_as_i64: i64 = pk;
    assert_eq!(pk_as_i64, 7);
}

/// `FieldSpec` is documented as `Copy + Eq`. Constructing two equal-by-value
/// instances and comparing them pins the derive down, and the `let copy = ...`
/// line wouldn't compile without `Copy`.
#[test]
fn field_spec_is_copy_and_eq() {
    let a = FieldSpec {
        name: "x",
        ty: SqlType::BigInt,
        primary_key: true,
        nullable: false,
        supported_backends: &[],
        fk_target: None,
        noform: false,
        privileged: false,
        db_constraint: true,
        noedit: false,
        is_string_repr: false,
        max_length: 0,
        choices: &[],
        choice_labels: &[],
        default: "",
        is_multichoice: false,
        unique: false,
        on_delete: umbral_core::orm::FkAction::NoAction,
        on_update: umbral_core::orm::FkAction::NoAction,
        index: false,
        auto_now_add: false,
        auto_now: false,
        trim: false,
        lowercase: false,
        help: "",
        example: "",
        widget: None,
        min: None,
        max: None,
        text_format: ::core::option::Option::None,
        slug_from: ::core::option::Option::None,
    };
    let b = FieldSpec {
        name: "x",
        ty: SqlType::BigInt,
        primary_key: true,
        nullable: false,
        supported_backends: &[],
        fk_target: None,
        noform: false,
        privileged: false,
        db_constraint: true,
        noedit: false,
        is_string_repr: false,
        max_length: 0,
        choices: &[],
        choice_labels: &[],
        default: "",
        is_multichoice: false,
        unique: false,
        on_delete: umbral_core::orm::FkAction::NoAction,
        on_update: umbral_core::orm::FkAction::NoAction,
        index: false,
        auto_now_add: false,
        auto_now: false,
        trim: false,
        lowercase: false,
        help: "",
        example: "",
        widget: None,
        min: None,
        max: None,
        text_format: ::core::option::Option::None,
        slug_from: ::core::option::Option::None,
    };

    assert_eq!(
        a, b,
        "FieldSpecs with identical fields should compare equal"
    );

    // Copy semantics: `a` stays usable after a move-by-value would have
    // consumed it. If `FieldSpec` weren't `Copy` this line would fail to
    // compile.
    let copy = a;
    assert_eq!(copy, a);
}

/// A different field name should make two `FieldSpec`s unequal. The Eq impl
/// would be useless if it ignored fields, so this guards the derive against
/// silently degrading to "always equal."
#[test]
fn field_spec_eq_distinguishes_different_names() {
    let a = FieldSpec {
        name: "x",
        ty: SqlType::BigInt,
        primary_key: false,
        nullable: false,
        supported_backends: &[],
        fk_target: None,
        noform: false,
        privileged: false,
        db_constraint: true,
        noedit: false,
        is_string_repr: false,
        max_length: 0,
        choices: &[],
        choice_labels: &[],
        default: "",
        is_multichoice: false,
        unique: false,
        on_delete: umbral_core::orm::FkAction::NoAction,
        on_update: umbral_core::orm::FkAction::NoAction,
        index: false,
        auto_now_add: false,
        auto_now: false,
        trim: false,
        lowercase: false,
        help: "",
        example: "",
        widget: None,
        min: None,
        max: None,
        text_format: ::core::option::Option::None,
        slug_from: ::core::option::Option::None,
    };
    let b = FieldSpec {
        name: "y",
        ty: SqlType::BigInt,
        primary_key: false,
        nullable: false,
        supported_backends: &[],
        fk_target: None,
        noform: false,
        privileged: false,
        db_constraint: true,
        noedit: false,
        is_string_repr: false,
        max_length: 0,
        choices: &[],
        choice_labels: &[],
        default: "",
        is_multichoice: false,
        unique: false,
        on_delete: umbral_core::orm::FkAction::NoAction,
        on_update: umbral_core::orm::FkAction::NoAction,
        index: false,
        auto_now_add: false,
        auto_now: false,
        trim: false,
        lowercase: false,
        help: "",
        example: "",
        widget: None,
        min: None,
        max: None,
        text_format: ::core::option::Option::None,
        slug_from: ::core::option::Option::None,
    };

    assert_ne!(a, b, "FieldSpecs differing in name must not compare equal");
}

/// `Post` satisfies `for<'r> FromRow<'r, SqliteRow>` via its `#[derive(sqlx::FromRow)]`,
/// which is the supertrait `Model` carries so QuerySet terminals can call
/// `sqlx::query_as::<_, T>` over any `T: Model`. This test runs an actual
/// SELECT through `query_as` to verify the row decoding works end-to-end
/// rather than just at the type level.
#[tokio::test]
async fn post_implements_fromrow() {
    let pool = db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite should always connect");

    sqlx::query(
        "CREATE TABLE post (\
             id INTEGER PRIMARY KEY AUTOINCREMENT,\
             title TEXT NOT NULL,\
             body TEXT NOT NULL,\
             published_at DATETIME\
         )",
    )
    .execute(&pool)
    .await
    .expect("CREATE TABLE post should succeed on a fresh in-memory database");

    sqlx::query("INSERT INTO post (id, title, body, published_at) VALUES (?, ?, ?, ?)")
        .bind(1_i64)
        .bind("hello")
        .bind("world")
        .bind::<Option<chrono::DateTime<chrono::Utc>>>(None)
        .execute(&pool)
        .await
        .expect("INSERT into post should succeed");

    let row: Post =
        sqlx::query_as::<_, Post>("SELECT id, title, body, published_at FROM post WHERE id = ?")
            .bind(1_i64)
            .fetch_one(&pool)
            .await
            .expect("query_as::<_, Post> should decode the row via the FromRow supertrait");

    assert_eq!(row.id, 1);
    assert_eq!(row.title, "hello");
    assert_eq!(row.body, "world");
    assert!(row.published_at.is_none());
    assert_eq!(
        row.primary_key(),
        1,
        "round-tripped row should expose its primary key via the trait method",
    );
}
