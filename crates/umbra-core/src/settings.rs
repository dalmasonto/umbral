use figment::Figment;
use figment::providers::{Env, Format, Toml};
use serde::Deserialize;
use std::sync::OnceLock;

/// Ambient settings, published during `AppBuilder::build()`.
static SETTINGS: OnceLock<Settings> = OnceLock::new();

/// Initialize ambient settings. Called by `AppBuilder::build()` only.
pub(crate) fn init(settings: &Settings) {
    // Clone the settings into the OnceLock. The struct is cheap to clone
    // (strings and vecs) and this avoids forcing the caller to surrender
    // ownership of the original.
    SETTINGS
        .set(settings.clone())
        .expect("umbra::settings::init called more than once");
}

/// Return a reference to the ambient settings.
///
/// # Panics
///
/// Panics if `App::build()` hasn't run.
pub fn get() -> &'static Settings {
    SETTINGS
        .get()
        .expect("umbra: settings not initialised — did you call App::build()?")
}

fn default_database_url() -> String {
    "sqlite://umbra.db".into()
}

fn default_secret_key() -> String {
    "umbra-insecure-dev-key-change-me".into()
}

fn default_allowed_hosts() -> Vec<String> {
    vec!["localhost".into(), "127.0.0.1".into()]
}

fn default_log_level() -> String {
    "info".into()
}

#[derive(Clone, Debug, Deserialize)]
pub struct Settings {
    #[serde(default = "default_database_url")]
    pub database_url: String,

    #[serde(default)]
    pub databases: std::collections::HashMap<String, String>,

    #[serde(default = "default_secret_key")]
    pub secret_key: String,

    #[serde(default)]
    pub environment: Environment,

    #[serde(default = "default_allowed_hosts")]
    pub allowed_hosts: Vec<String>,

    #[serde(default = "default_log_level")]
    pub log_level: String,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub enum Environment {
    #[default]
    Dev,
    Test,
    Prod,
}

impl Settings {
    /// Load settings from defaults, `umbra.toml`, and `UMBRA_`-prefixed env vars.
    ///
    /// Precedence (later wins): struct defaults → `umbra.toml` → env vars.
    ///
    /// The error type is boxed because `figment::Error` is large (over 200
    /// bytes); see `clippy::result_large_err`.
    pub fn from_env() -> Result<Self, Box<figment::Error>> {
        Figment::new()
            .join(Toml::file("umbra.toml"))
            .join(Env::prefixed("UMBRA_").split("__"))
            .extract()
            .map_err(Box::new)
    }
}
