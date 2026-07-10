//! gaps3 #38 — `typegen::typescript()` reads the live model registry.
//!
//! `typegen.rs` covers the pure generator. This covers the wiring the CLI's
//! `typegen` subcommand actually uses: models registered on the app via
//! `.model::<T>()` AND models contributed by a plugin, all resolved against each
//! other so a cross-plugin foreign key still types as its target's primary key.
//!
//! `App::build()` publishes process-global state through `OnceLock`s, so this
//! spends the one successful build a test binary gets.

use std::path::PathBuf;

use umbral::migrate::ModelMeta;
use umbral::orm::ForeignKey;
use umbral::plugin::Plugin;

/// Owned by the `tgreg_accounts` plugin.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "tgreg_account")]
pub struct TgRegAccount {
    #[umbral(primary_key)]
    pub handle: String,
    pub email: String,
}

/// Registered on the app itself, with a cross-plugin FK into the plugin's table.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "tgreg_note")]
pub struct TgRegNote {
    pub id: i64,
    pub body: String,
    #[umbral(no_reverse)]
    pub owner: ForeignKey<TgRegAccount>,
}

#[derive(Debug, Default, Clone)]
struct AccountsPlugin;

impl Plugin for AccountsPlugin {
    fn name(&self) -> &'static str {
        "tgreg_accounts"
    }
    fn models(&self) -> Vec<ModelMeta> {
        vec![ModelMeta::for_::<TgRegAccount>()]
    }
    fn templates_dirs(&self) -> Vec<PathBuf> {
        Vec::new()
    }
}

#[tokio::test]
async fn typescript_reads_every_registered_model_across_plugins() {
    let settings = umbral::Settings::from_env().expect("figment defaults load in a test env");
    let pool = umbral::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite connects");

    umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .plugin(AccountsPlugin)
        .model::<TgRegNote>()
        .build()
        .expect("App::build");

    let ts = umbral::typegen::typescript();

    // Both the plugin's model and the app's model are present.
    assert!(
        ts.contains("export interface TgRegAccount {"),
        "the plugin's model must be generated:\n{ts}",
    );
    assert!(
        ts.contains("export interface TgRegNote {"),
        "the app's model must be generated:\n{ts}",
    );

    // The cross-plugin FK resolves to the target's String primary key, and the
    // doc comment names the real PK column (`handle`, not `id`).
    assert!(
        ts.contains("  owner: string;"),
        "a cross-plugin FK must type as its target's PK:\n{ts}",
    );
    assert!(
        ts.contains("Foreign key: the `handle` of a TgRegAccount (`tgreg_account`)."),
        "the FK doc comment must name the real PK column:\n{ts}",
    );

    // Registry order is by model name, so regenerating never reshuffles the file.
    let account_at = ts.find("interface TgRegAccount").expect("account present");
    let note_at = ts.find("interface TgRegNote").expect("note present");
    assert!(
        account_at < note_at,
        "models are emitted sorted by name so the output is stable:\n{ts}",
    );
}
