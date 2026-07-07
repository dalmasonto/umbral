//! gaps2 #70 — text-backed Postgres-only field types: XML, LTREE,
//! BIT VARYING, plus the (already-free) MACADDR explicit marker.
//!
//! Coverage layers mirror `network_field.rs`:
//!
//! - **Derive classification.** `#[umbral(xml)]` / `#[umbral(ltree)]` /
//!   `#[umbral(bit)]` upgrade a `String` / `Option<String>` field from
//!   `SqlType::Text` to the native PG type without changing the Rust
//!   type. A plain `String` field (no attr) stays `Text` (regression
//!   guard). `#[umbral(macaddr)]` confirms a `mac_address::MacAddress`
//!   field classifies as `MacAddr` (already the auto-detected default —
//!   the attribute is a no-op marker that documents intent).
//! - **DDL rendering.** Postgres emits `xml` / `ltree` / `bit varying`
//!   column types.
//! - **Column constants.** The right `*Col` / `Nullable*Col` types are
//!   exposed.
//! - **inspectdb.** The PG type names round-trip back to `String`.
//! - **Backend gating.** An `#[umbral(xml)]` field against SQLite fails
//!   at boot with `field.backend` (ignored — pollutes the registry).
//! - **Live PG round-trip** behind `#[ignore]`.

use umbral::orm::{Model, SqlType};
use umbral_core::migrate::{Column, Operation, render_operation_for};

#[derive(Debug, Clone, sqlx::FromRow, umbral::orm::Model)]
#[umbral(table = "umbral_gaps2_70_doc")]
pub struct Doc {
    pub id: i64,
    /// `#[umbral(xml)]` → `SqlType::Xml` (not Text).
    #[umbral(xml)]
    pub body: String,
    /// `#[umbral(ltree)]` on a non-nullable field → `SqlType::Ltree`.
    #[umbral(ltree)]
    pub path: String,
    /// `#[umbral(bit)]` → `SqlType::Bit` (BIT VARYING).
    #[umbral(bit)]
    pub flags: String,
    /// Nullable XML: `#[umbral(xml)]` on `Option<String>` → nullable Xml.
    #[umbral(xml)]
    pub note: Option<String>,
    /// `#[umbral(macaddr)]` confirms MacAddr (already auto-detected).
    #[umbral(macaddr)]
    pub mac: mac_address::MacAddress,
    /// Plain `String` (no attr) — regression guard; must stay `Text`.
    pub title: String,
}

fn field_map() -> std::collections::HashMap<&'static str, &'static umbral::orm::FieldSpec> {
    <Doc as Model>::FIELDS.iter().map(|f| (f.name, f)).collect()
}

#[test]
fn xml_attr_classifies_as_xml_sqltype() {
    let by_name = field_map();

    let body = by_name.get("body").expect("body field");
    assert_eq!(body.ty, SqlType::Xml, "#[umbral(xml)] String → Xml");
    assert!(!body.nullable);

    let note = by_name.get("note").expect("note field");
    assert_eq!(
        note.ty,
        SqlType::Xml,
        "#[umbral(xml)] Option<String> → nullable Xml"
    );
    assert!(note.nullable);
}

#[test]
fn ltree_and_bit_attrs_classify_correctly() {
    let by_name = field_map();

    let path = by_name.get("path").expect("path field");
    assert_eq!(path.ty, SqlType::Ltree, "#[umbral(ltree)] String → Ltree");
    assert!(!path.nullable);

    let flags = by_name.get("flags").expect("flags field");
    assert_eq!(flags.ty, SqlType::Bit, "#[umbral(bit)] String → Bit");
    assert!(!flags.nullable);
}

/// `#[umbral(macaddr)]` is the "free" type — the `mac_address::MacAddress`
/// field already detects to `MacAddr`; the attribute only documents intent
/// and must not re-classify the field.
#[test]
fn macaddr_marker_is_macaddr_sqltype() {
    let by_name = field_map();
    let mac = by_name.get("mac").expect("mac field");
    assert_eq!(mac.ty, SqlType::MacAddr);
    assert!(!mac.nullable);
}

/// Regression: a plain `String` field without any attr stays `Text`.
#[test]
fn plain_string_field_stays_text() {
    let by_name = field_map();
    let title = by_name.get("title").expect("title field");
    assert_eq!(
        title.ty,
        SqlType::Text,
        "String without an attr must remain Text"
    );
}

fn col(name: &str, ty: SqlType, nullable: bool) -> Column {
    Column {
        name: name.to_string(),
        ty,
        primary_key: ty == SqlType::BigInt && name == "id",
        nullable,
        fk_target: None,
        noform: false,
        privileged: false,
        db_constraint: true,
        noedit: false,
        is_string_repr: false,
        max_length: 0,
        choices: Vec::new(),
        choice_labels: Vec::new(),
        default: String::new(),
        is_multichoice: false,
        unique: false,
        on_delete: umbral_core::orm::FkAction::NoAction,
        on_update: umbral_core::orm::FkAction::NoAction,
        index: false,
        auto_now_add: false,
        auto_now: false,
        trim: false,
        lowercase: false,
        help: String::new(),
        example: String::new(),
        widget: None,
        supported_backends: Vec::new(),
        min: None,
        max: None,
        text_format: ::core::option::Option::None,
        slug_from: ::core::option::Option::None,
    }
}

#[test]
fn postgres_ddl_renders_xml_ltree_bit_types() {
    let op = Operation::CreateTable {
        table: "umbral_gaps2_70_doc".to_string(),
        columns: vec![
            col("id", SqlType::BigInt, false),
            col("body", SqlType::Xml, false),
            col("path", SqlType::Ltree, false),
            col("flags", SqlType::Bit, false),
            col("note", SqlType::Xml, true),
        ],
        unique_together: Vec::new(),
        indexes: Vec::new(),
    };

    let stmts = render_operation_for(&op, "postgres");
    let sql = &stmts[0];
    let lower = sql.to_ascii_lowercase();
    assert!(lower.contains("xml"), "expected `xml`; got {sql}");
    assert!(lower.contains("ltree"), "expected `ltree`; got {sql}");
    assert!(
        lower.contains("bit varying") || lower.contains("varbit"),
        "expected `bit varying`; got {sql}"
    );
}

#[test]
fn column_const_module_has_text_pg_types() {
    use umbral::orm::column::{BitCol, LtreeCol, MacAddrCol, NullableXmlCol, XmlCol};
    let _: XmlCol<Doc> = doc::BODY;
    let _: LtreeCol<Doc> = doc::PATH;
    let _: BitCol<Doc> = doc::FLAGS;
    let _: NullableXmlCol<Doc> = doc::NOTE;
    let _: MacAddrCol<Doc> = doc::MAC;
}

/// inspectdb renders the text-backed PG types back to `String` (the attr
/// can't be recovered from the DB type alone, so the porting on-ramp
/// emits a plain `String` field).
#[test]
fn inspect_renders_text_pg_types_as_string() {
    use umbral::inspect::{
        IntrospectedColumn, IntrospectedSchema, IntrospectedTable, render_models,
    };
    let schema = IntrospectedSchema {
        tables: vec![IntrospectedTable {
            table: "umbral_gaps2_70_doc".to_string(),
            name: "Doc".to_string(),
            columns: vec![
                IntrospectedColumn {
                    name: "id".to_string(),
                    ty: SqlType::BigInt,
                    primary_key: true,
                    nullable: false,
                },
                IntrospectedColumn {
                    name: "body".to_string(),
                    ty: SqlType::Xml,
                    primary_key: false,
                    nullable: false,
                },
                IntrospectedColumn {
                    name: "path".to_string(),
                    ty: SqlType::Ltree,
                    primary_key: false,
                    nullable: false,
                },
                IntrospectedColumn {
                    name: "flags".to_string(),
                    ty: SqlType::Bit,
                    primary_key: false,
                    nullable: true,
                },
            ],
        }],
    };
    let out = render_models(&schema);
    assert!(
        out.contains("pub body: String,"),
        "Xml should render as String; got:\n{out}"
    );
    assert!(
        out.contains("pub path: String,"),
        "Ltree should render as String; got:\n{out}"
    );
    assert!(
        out.contains("pub flags: Option<String>,"),
        "nullable Bit should render as Option<String>; got:\n{out}"
    );
}

/// The boot system check rejects a text-backed PG-only field against
/// SQLite the same way it rejects Inet / Cidr / Array / Decimal.
#[tokio::test]
#[ignore = "pollutes the process-wide model registry; run isolated"]
async fn field_backend_rejects_xml_on_sqlite() {
    use umbral::{App, Settings};
    use umbral_core::app::BuildError;

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
        Ok(_) => panic!("expected build to fail on xml+sqlite"),
    }
}

/// Live Postgres round-trip for all three text-backed types. Requires the
/// `ltree` extension in the target DB. Behind `#[ignore]`.
#[tokio::test]
#[ignore = "needs UMBRAL_TEST_POSTGRES_URL + CREATE EXTENSION ltree"]
async fn text_pg_fields_round_trip_through_postgres() {
    let url =
        std::env::var("UMBRAL_TEST_POSTGRES_URL").expect("UMBRAL_TEST_POSTGRES_URL must be set");
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    sqlx::query("CREATE EXTENSION IF NOT EXISTS ltree")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DROP TABLE IF EXISTS umbral_gaps2_70_doc")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE umbral_gaps2_70_doc ( \
            id BIGSERIAL PRIMARY KEY, \
            body XML NOT NULL, \
            path LTREE NOT NULL, \
            flags BIT VARYING NOT NULL, \
            note XML, \
            mac MACADDR NOT NULL, \
            title TEXT NOT NULL \
         )",
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO umbral_gaps2_70_doc (body, path, flags, note, mac, title) \
         VALUES ($1::xml, $2::ltree, $3::bit varying, $4::xml, $5::macaddr, $6)",
    )
    .bind("<a>hi</a>")
    .bind("Top.Science")
    .bind("101")
    .bind(Some("<n/>"))
    .bind("aa:bb:cc:dd:ee:ff")
    .bind("hello")
    .execute(&pool)
    .await
    .unwrap();

    // Query via the ORM filter surface to exercise the *Col predicates.
    let rows = Doc::objects()
        .filter(doc::PATH.eq("Top.Science"))
        .fetch_pg(&pool)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].path, "Top.Science");
    assert_eq!(rows[0].flags, "101");
    assert_eq!(rows[0].title, "hello");
    assert_eq!(rows[0].note.as_deref(), Some("<n/>"));
}
