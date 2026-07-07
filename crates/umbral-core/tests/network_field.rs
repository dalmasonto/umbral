//! Phase 4.4 — Postgres network address field types.
//!
//! Coverage layers:
//!
//! - **Derive classification.** `ipnetwork::IpNetwork` lands as
//!   `SqlType::Inet`; `Option<IpNetwork>` as nullable. Same for
//!   `mac_address::MacAddress` → `SqlType::MacAddr`.
//! - **`#[umbral(cidr)]` opt-in.** Closes the derive-reachable half of
//!   gaps2 #70: a `#[umbral(cidr)]` field switches `IpNetwork` /
//!   `Option<IpNetwork>` from Inet → Cidr without changing the Rust type.
//!   Without the attr it stays Inet (regression guard).
//! - **Backend gating.** Inet/Cidr/MacAddr against SQLite fails at
//!   boot with `field.backend`.
//! - **DDL rendering.** Postgres emits `inet` / `macaddr` column
//!   types.
//! - **Type-level pin.** Column constants expose the right types.
//! - **Live PG round-trip** behind `#[ignore]`.

use umbral::orm::{Model, SqlType};

#[derive(Debug, Clone, sqlx::FromRow, umbral::orm::Model)]
#[umbral(table = "umbral_phase44_node")]
pub struct Node {
    pub id: i64,
    pub addr: ipnetwork::IpNetwork,
    pub mac: mac_address::MacAddress,
    pub fallback: Option<ipnetwork::IpNetwork>,
}

/// A model that exercises `#[umbral(cidr)]` on both non-nullable and nullable
/// `IpNetwork` fields. The Rust type is identical to `Inet`; only the
/// `SqlType` (and therefore the DDL / inspectdb output) differs.
#[derive(Debug, Clone, sqlx::FromRow, umbral::orm::Model)]
#[umbral(table = "umbral_phase44_subnet")]
pub struct Subnet {
    pub id: i64,
    /// Non-nullable CIDR: `#[umbral(cidr)]` → `SqlType::Cidr`, not `Inet`.
    #[umbral(cidr)]
    pub network: ipnetwork::IpNetwork,
    /// Nullable CIDR: `#[umbral(cidr)]` on `Option<IpNetwork>` → nullable Cidr.
    #[umbral(cidr)]
    pub fallback_net: Option<ipnetwork::IpNetwork>,
    /// Plain INET (no attr) — regression guard; must stay `SqlType::Inet`.
    pub gateway: ipnetwork::IpNetwork,
}

#[test]
fn derive_classifies_ipnetwork_as_inet_sqltype() {
    let by_name: std::collections::HashMap<&str, &umbral::orm::FieldSpec> = <Node as Model>::FIELDS
        .iter()
        .map(|f| (f.name, f))
        .collect();

    let addr = by_name.get("addr").expect("addr field");
    assert_eq!(addr.ty, SqlType::Inet);
    assert!(!addr.nullable);

    let fallback = by_name.get("fallback").expect("fallback field");
    assert_eq!(fallback.ty, SqlType::Inet);
    assert!(fallback.nullable);
}

#[test]
fn derive_classifies_mac_address_as_macaddr_sqltype() {
    let by_name: std::collections::HashMap<&str, &umbral::orm::FieldSpec> = <Node as Model>::FIELDS
        .iter()
        .map(|f| (f.name, f))
        .collect();

    let mac = by_name.get("mac").expect("mac field");
    assert_eq!(mac.ty, SqlType::MacAddr);
    assert!(!mac.nullable);
}

#[test]
fn postgres_ddl_renders_inet_and_macaddr_types() {
    use umbral::migrate::{Column, Operation, render_operation_for};

    let op = Operation::CreateTable {
        table: "umbral_phase44_node".to_string(),
        columns: vec![
            Column {
                name: "id".to_string(),
                ty: SqlType::BigInt,
                primary_key: true,
                nullable: false,
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
                case_insensitive: false,
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
                name: "addr".to_string(),
                ty: SqlType::Inet,
                primary_key: false,
                nullable: false,
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
                case_insensitive: false,
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
                name: "mac".to_string(),
                ty: SqlType::MacAddr,
                primary_key: false,
                nullable: false,
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
                case_insensitive: false,
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
                name: "net".to_string(),
                ty: SqlType::Cidr,
                primary_key: false,
                nullable: true,
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
                case_insensitive: false,
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
    let sql = &stmts[0];
    let lower = sql.to_ascii_lowercase();
    assert!(lower.contains("inet"), "expected `inet`; got {sql}");
    assert!(lower.contains("macaddr"), "expected `macaddr`; got {sql}");
    assert!(lower.contains("cidr"), "expected `cidr`; got {sql}");
}

#[test]
fn column_const_module_has_network_types() {
    use umbral::orm::column::{InetCol, MacAddrCol, NullableInetCol};
    let _: InetCol<Node> = node::ADDR;
    let _: MacAddrCol<Node> = node::MAC;
    let _: NullableInetCol<Node> = node::FALLBACK;
}

/// Inspect's map_postgres_type recognises inet / cidr / macaddr.
#[test]
fn inspect_maps_postgres_network_types() {
    // Re-test through the public inspect surface — `introspect_pool_pg`
    // is the entry; map_postgres_type is internal. We exercise the
    // public surface that uses it by reading the type name through
    // render_field_type via SqlType.
    //
    // (The internal `map_postgres_type` is covered in the inspect
    // unit tests in umbral-core. This integration-level pin just
    // verifies the SqlType variants round-trip to the right Rust
    // type strings in the generated models output.)
    use umbral::inspect::{
        IntrospectedColumn, IntrospectedSchema, IntrospectedTable, render_models,
    };
    let schema = IntrospectedSchema {
        tables: vec![IntrospectedTable {
            table: "umbral_phase44_node".to_string(),
            name: "Node".to_string(),
            columns: vec![
                IntrospectedColumn {
                    name: "id".to_string(),
                    ty: SqlType::BigInt,
                    primary_key: true,
                    nullable: false,
                },
                IntrospectedColumn {
                    name: "addr".to_string(),
                    ty: SqlType::Inet,
                    primary_key: false,
                    nullable: false,
                },
                IntrospectedColumn {
                    name: "mac".to_string(),
                    ty: SqlType::MacAddr,
                    primary_key: false,
                    nullable: false,
                },
            ],
        }],
    };
    let out = render_models(&schema);
    assert!(
        out.contains("pub addr: ipnetwork::IpNetwork,"),
        "Inet should render as ipnetwork::IpNetwork; got:\n{out}"
    );
    assert!(
        out.contains("pub mac: mac_address::MacAddress,"),
        "MacAddr should render as mac_address::MacAddress; got:\n{out}"
    );
}

#[tokio::test]
#[ignore = "pollutes the process-wide model registry; run isolated"]
async fn field_backend_rejects_inet_on_sqlite() {
    use umbral::{App, Settings};
    use umbral_core::app::BuildError;

    let mut settings = Settings::from_env().expect("figment defaults load");
    settings.database_url = "sqlite::memory:".to_string();
    let sqlite_pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();

    let result = App::builder()
        .settings(settings)
        .database("default", sqlite_pool)
        .model::<Node>()
        .build();

    match result {
        Err(BuildError::SystemCheckFailed { findings }) => {
            let has = findings.iter().any(|f| f.check_id == "field.backend");
            assert!(has, "expected field.backend finding; got {findings:?}");
        }
        Err(other) => panic!("expected SystemCheckFailed, got {other:?}"),
        Ok(_) => panic!("expected build to fail on inet+sqlite"),
    }
}

#[tokio::test]
#[ignore = "needs UMBRAL_TEST_POSTGRES_URL"]
async fn network_fields_round_trip_through_postgres() {
    use std::str::FromStr;
    let url =
        std::env::var("UMBRAL_TEST_POSTGRES_URL").expect("UMBRAL_TEST_POSTGRES_URL must be set");
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    sqlx::query("DROP TABLE IF EXISTS umbral_phase44_node")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE umbral_phase44_node ( \
            id BIGSERIAL PRIMARY KEY, \
            addr INET NOT NULL, \
            mac MACADDR NOT NULL, \
            fallback INET \
         )",
    )
    .execute(&pool)
    .await
    .unwrap();

    let primary = ipnetwork::IpNetwork::from_str("10.0.0.1/24").unwrap();
    let backup = ipnetwork::IpNetwork::from_str("192.168.1.1/24").unwrap();
    let mac = mac_address::MacAddress::from_str("aa:bb:cc:dd:ee:ff").unwrap();

    sqlx::query("INSERT INTO umbral_phase44_node (addr, mac, fallback) VALUES ($1, $2, $3)")
        .bind(primary)
        .bind(mac)
        .bind(Some(backup))
        .execute(&pool)
        .await
        .unwrap();

    let rows = Node::objects().fetch_pg(&pool).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].addr, primary);
    assert_eq!(rows[0].mac, mac);
    assert_eq!(rows[0].fallback, Some(backup));
}

// =============================================================================
// `#[umbral(cidr)]` derive attribute — closes the derive-reachable half of
// gaps2 #70.
// =============================================================================

/// `#[umbral(cidr)]` on a non-nullable `IpNetwork` field classifies as
/// `SqlType::Cidr` (not `Inet`). A plain `IpNetwork` field without the
/// attribute stays `Inet` (regression guard).
#[test]
fn cidr_attr_classifies_as_cidr_sqltype() {
    let by_name: std::collections::HashMap<&str, &umbral::orm::FieldSpec> =
        <Subnet as Model>::FIELDS
            .iter()
            .map(|f| (f.name, f))
            .collect();

    let network = by_name.get("network").expect("network field");
    assert_eq!(
        network.ty,
        SqlType::Cidr,
        "#[umbral(cidr)] IpNetwork should classify as Cidr"
    );
    assert!(!network.nullable);

    let fallback_net = by_name.get("fallback_net").expect("fallback_net field");
    assert_eq!(
        fallback_net.ty,
        SqlType::Cidr,
        "#[umbral(cidr)] Option<IpNetwork> should classify as Cidr"
    );
    assert!(fallback_net.nullable);

    // Regression: plain IpNetwork (no attr) must remain Inet.
    let gateway = by_name.get("gateway").expect("gateway field");
    assert_eq!(
        gateway.ty,
        SqlType::Inet,
        "IpNetwork without #[umbral(cidr)] must remain Inet"
    );
    assert!(!gateway.nullable);
}

/// Column constants for a model with `#[umbral(cidr)]` fields expose the
/// `CidrCol` / `NullableCidrCol` types (not `InetCol`).
#[test]
fn cidr_attr_produces_cidr_col_constants() {
    use umbral::orm::column::{CidrCol, InetCol, NullableCidrCol};
    let _: CidrCol<Subnet> = subnet::NETWORK;
    let _: NullableCidrCol<Subnet> = subnet::FALLBACK_NET;
    let _: InetCol<Subnet> = subnet::GATEWAY;
}
