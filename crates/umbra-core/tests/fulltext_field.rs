//! Phase 4.3 — Postgres full-text search field type.
//!
//! Coverage layers:
//!
//! - **Derive classification.** `umbra::orm::TsVector` lands as
//!   `SqlType::FullText`; `Option<TsVector>` as nullable.
//! - **Backend gating.** FullText against SQLite fails at boot via
//!   `field.backend`.
//! - **DDL rendering.** Postgres emits `tsvector`.
//! - **Operators.** `.matches(query)` renders as `@@ to_tsquery($1)`;
//!   `.matches_websearch(query)` as `@@ websearch_to_tsquery($1)`.
//! - **Live PG round-trip** behind `#[ignore]`.

use umbra::orm::{Model, SqlType, TsVector};

#[derive(Debug, Clone, sqlx::FromRow, umbra::orm::Model)]
#[umbra(table = "umbra_phase43_doc")]
pub struct Doc {
    pub id: i64,
    pub title: String,
    pub search: umbra::orm::TsVector,
    pub alt_search: Option<umbra::orm::TsVector>,
}

#[test]
fn derive_classifies_tsvector_as_fulltext_sqltype() {
    let by_name: std::collections::HashMap<&str, &umbra::orm::FieldSpec> =
        <Doc as Model>::FIELDS.iter().map(|f| (f.name, f)).collect();

    let search = by_name.get("search").expect("search field");
    assert_eq!(search.ty, SqlType::FullText);
    assert!(!search.nullable);

    let alt = by_name.get("alt_search").expect("alt_search field");
    assert_eq!(alt.ty, SqlType::FullText);
    assert!(alt.nullable, "Option<TsVector> is the nullable variant");
}

#[test]
fn postgres_ddl_renders_tsvector_type() {
    use umbra::migrate::{Column, Operation, render_operation_for};

    let op = Operation::CreateTable {
        table: "umbra_phase43_doc".to_string(),
        columns: vec![
            Column {
                name: "id".to_string(),
                ty: SqlType::BigInt,
                primary_key: true,
                nullable: false,
                fk_target: None,
                noform: false,
                noedit: false,
                is_string_repr: false,
                max_length: 0,
                choices: Vec::new(),
                choice_labels: Vec::new(),
                default: String::new(),
                is_multichoice: false,
                unique: false,
                on_delete: umbra_core::orm::FkAction::NoAction,
                on_update: umbra_core::orm::FkAction::NoAction,
                index: false,
                auto_now_add: false,
                auto_now: false,
                help: String::new(),
                example: String::new(),
                widget: None,
                supported_backends: Vec::new(),
                min: None,
                max: None,
                text_format: ::core::option::Option::None,
                slug_from: ::core::option::Option::None,
            },
            Column {
                name: "search".to_string(),
                ty: SqlType::FullText,
                primary_key: false,
                nullable: false,
                fk_target: None,
                noform: false,
                noedit: false,
                is_string_repr: false,
                max_length: 0,
                choices: Vec::new(),
                choice_labels: Vec::new(),
                default: String::new(),
                is_multichoice: false,
                unique: false,
                on_delete: umbra_core::orm::FkAction::NoAction,
                on_update: umbra_core::orm::FkAction::NoAction,
                index: false,
                auto_now_add: false,
                auto_now: false,
                help: String::new(),
                example: String::new(),
                widget: None,
                supported_backends: Vec::new(),
                min: None,
                max: None,
                text_format: ::core::option::Option::None,
                slug_from: ::core::option::Option::None,
            },
        ],
        unique_together: Vec::new(),
        indexes: Vec::new(),
    };
    let stmts = render_operation_for(&op, "postgres");
    let lower = stmts[0].to_ascii_lowercase();
    assert!(
        lower.contains("tsvector"),
        "expected `tsvector`; got {}",
        stmts[0]
    );

    // #33 — the tsvector column gets an auto-GIN index in a follow-up
    // statement (a tsvector column is useless for search without one).
    let all = stmts.join("\n").to_ascii_lowercase();
    assert!(
        all.contains("using gin"),
        "expected an auto-GIN index for the tsvector column; got {stmts:?}"
    );
    assert!(
        all.contains("idx_umbra_phase43_doc_search_gin"),
        "GIN index named per the idx_<table>_<col>_gin convention; got {stmts:?}"
    );
    // The id column has no `index` flag, so it gets no extra index — only
    // the GIN one is added beyond the CREATE TABLE.
    assert_eq!(
        stmts.len(),
        2,
        "exactly CREATE TABLE + the GIN index; got {stmts:?}"
    );
}

#[test]
fn matches_renders_at_at_to_tsquery() {
    let qs = Doc::objects().filter(doc::SEARCH.matches("alice & bob"));
    let sql = qs.to_sql_pg();
    assert!(sql.contains("@@"), "expected @@ operator; got {sql}");
    assert!(
        sql.contains("to_tsquery"),
        "expected to_tsquery function; got {sql}"
    );
    assert!(
        !sql.contains("websearch_to_tsquery"),
        "matches() should not emit websearch variant; got {sql}"
    );
}

#[test]
fn matches_websearch_renders_websearch_to_tsquery() {
    let qs = Doc::objects().filter(doc::SEARCH.matches_websearch("alice OR \"bob smith\""));
    let sql = qs.to_sql_pg();
    assert!(sql.contains("@@"), "expected @@; got {sql}");
    assert!(
        sql.contains("websearch_to_tsquery"),
        "expected websearch_to_tsquery; got {sql}"
    );
}

#[test]
fn nullable_fulltext_col_supports_matches_and_is_null() {
    let qs1 = Doc::objects().filter(doc::ALT_SEARCH.matches("alpha"));
    let sql1 = qs1.to_sql_pg();
    assert!(sql1.contains("@@") && sql1.contains("to_tsquery"));

    let qs2 = Doc::objects().filter(doc::ALT_SEARCH.is_null());
    let sql2 = qs2.to_sql_pg();
    assert!(sql2.contains("IS NULL"));
}

#[test]
fn column_const_module_has_fulltext_types() {
    use umbra::orm::column::{FullTextCol, NullableFullTextCol};
    let _: FullTextCol<Doc> = doc::SEARCH;
    let _: NullableFullTextCol<Doc> = doc::ALT_SEARCH;
}

#[test]
fn tsvector_newtype_round_trips_string() {
    let v = TsVector::from("'hello':1 'world':2");
    assert_eq!(v.as_str(), "'hello':1 'world':2");
    assert_eq!(v.clone().into_inner(), "'hello':1 'world':2");
    let v2: TsVector = "lex".into();
    assert_eq!(v2.as_ref(), "lex");
}

#[tokio::test]
#[ignore = "pollutes the process-wide model registry; run isolated"]
async fn field_backend_rejects_fulltext_on_sqlite() {
    use umbra::{App, Settings};
    use umbra_core::app::BuildError;

    let mut settings = Settings::from_env().expect("figment defaults load");
    settings.database_url = "sqlite::memory:".to_string();
    let sqlite_pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();

    let result = App::builder()
        .settings(settings)
        .database("default", sqlite_pool)
        .model::<Doc>()
        .build();

    match result {
        Err(BuildError::SystemCheckFailed { findings }) => {
            let has = findings.iter().any(|f| f.check_id == "field.backend");
            assert!(has, "expected field.backend finding; got {findings:?}");
        }
        Err(other) => panic!("expected SystemCheckFailed, got {other:?}"),
        Ok(_) => panic!("expected build to fail on fulltext+sqlite"),
    }
}

#[tokio::test]
#[ignore = "needs UMBRA_TEST_POSTGRES_URL"]
async fn fulltext_field_filters_real_postgres_rows() {
    let url =
        std::env::var("UMBRA_TEST_POSTGRES_URL").expect("UMBRA_TEST_POSTGRES_URL must be set");
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    sqlx::query("DROP TABLE IF EXISTS umbra_phase43_doc")
        .execute(&pool)
        .await
        .unwrap();
    // Populate `search` via GENERATED ALWAYS — the natural way to use
    // tsvector columns in production.
    sqlx::query(
        "CREATE TABLE umbra_phase43_doc ( \
            id BIGSERIAL PRIMARY KEY, \
            title TEXT NOT NULL, \
            search TSVECTOR GENERATED ALWAYS AS (to_tsvector('english', title)) STORED, \
            alt_search TSVECTOR \
         )",
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query("INSERT INTO umbra_phase43_doc (title) VALUES ($1)")
        .bind("The quick brown fox jumps over the lazy dog")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO umbra_phase43_doc (title) VALUES ($1)")
        .bind("Rust web framework comparison")
        .execute(&pool)
        .await
        .unwrap();

    let fox_only = Doc::objects()
        .filter(doc::SEARCH.matches("fox & dog"))
        .fetch_pg(&pool)
        .await
        .unwrap();
    assert_eq!(fox_only.len(), 1);
    assert!(fox_only[0].title.contains("fox"));

    let rust_via_websearch = Doc::objects()
        .filter(doc::SEARCH.matches_websearch("rust framework"))
        .fetch_pg(&pool)
        .await
        .unwrap();
    assert_eq!(rust_via_websearch.len(), 1);
    assert!(rust_via_websearch[0].title.to_lowercase().contains("rust"));
}

/// Documents the semantics that back the FTS docs (and answer the common
/// "will `?search=tseb` match `best`?" question): full-text search matches
/// LEXEMES (whole, stemmed words + prefixes), NOT substrings and NOT
/// reversed strings. `best product` matches; `tseb` matches nothing.
#[tokio::test]
#[ignore = "needs UMBRA_TEST_POSTGRES_URL"]
async fn fts_matches_lexemes_not_substrings_or_reverses() {
    let url =
        std::env::var("UMBRA_TEST_POSTGRES_URL").expect("UMBRA_TEST_POSTGRES_URL must be set");
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    sqlx::query("DROP TABLE IF EXISTS umbra_phase43_doc")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE umbra_phase43_doc ( \
            id BIGSERIAL PRIMARY KEY, \
            title TEXT NOT NULL, \
            search TSVECTOR GENERATED ALWAYS AS (to_tsvector('english', title)) STORED, \
            alt_search TSVECTOR \
         )",
    )
    .execute(&pool)
    .await
    .unwrap();
    for title in [
        "The best product ever made",
        "Best practices guide",
        "A great widget tool",
    ] {
        sqlx::query("INSERT INTO umbra_phase43_doc (title) VALUES ($1)")
            .bind(title)
            .execute(&pool)
            .await
            .unwrap();
    }

    let count = |pred| {
        let pool = pool.clone();
        async move {
            Doc::objects()
                .filter(pred)
                .fetch_pg(&pool)
                .await
                .unwrap()
                .len()
        }
    };

    // "best product": spaces mean AND → docs containing BOTH lexemes.
    // Only "The best product ever made" has both.
    assert_eq!(
        count(doc::SEARCH.matches_websearch("best product")).await,
        1
    );
    // "best" alone (stemmed, case-insensitive) hits both "best …" rows.
    assert_eq!(count(doc::SEARCH.matches_websearch("best")).await, 2);
    // "tseb" (best reversed) is not a lexeme in any doc → ZERO matches.
    // FTS does not reverse or fuzzy-match; this is the key clarification.
    assert_eq!(count(doc::SEARCH.matches_websearch("tseb")).await, 0);
    // A real substring of a word ("rodu" inside "product") is ALSO not a
    // lexeme → ZERO. FTS is word-based, not substring-based.
    assert_eq!(count(doc::SEARCH.matches_websearch("rodu")).await, 0);
    // Prefix matching is the supported "partial word": `prod:*` → product.
    assert_eq!(count(doc::SEARCH.matches("prod:*")).await, 1);
}
