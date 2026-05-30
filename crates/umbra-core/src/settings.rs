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
    // In-memory SQLite so first-run with all defaults works without any
    // filesystem assumptions (a sqlite:// URL pointing at a non-existent
    // file errors out without `?mode=rwc`). Real apps override this via
    // umbra.toml or UMBRA_DATABASE_URL.
    "sqlite::memory:".into()
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

fn default_bind_addr() -> String {
    // 127.0.0.1 only by default — exposing the server on 0.0.0.0
    // is a deliberate keystroke. Override with UMBRA_BIND_ADDR or
    // umbra.toml.
    "127.0.0.1:8000".into()
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

    /// The address the development server binds to.
    /// `host:port` format, e.g. `127.0.0.1:8000` (default), `0.0.0.0:80`,
    /// `[::1]:8000`. Override with `UMBRA_BIND_ADDR` or `umbra.toml`.
    #[serde(default = "default_bind_addr")]
    pub bind_addr: String,

    /// Catch-all for `UMBRA_`-prefixed environment variables (and
    /// `umbra.toml` keys) that don't map to a named field above.
    ///
    /// Real apps usually need keys the framework doesn't know about —
    /// `OPENAI_API_KEY`, `STRIPE_SECRET`, third-party plugin
    /// configuration. Setting `UMBRA_OPENAI_API_KEY=sk-test` makes
    /// `settings.extra.get("openai_api_key")` return a string value
    /// without the user crate having to wire a second figment loader.
    ///
    /// Values are stored as `toml::Value` so a nested
    /// `[external.openai]` table in `umbra.toml` round-trips with its
    /// structure intact. The accessor [`Settings::extra_str`] handles
    /// the common scalar-string case.
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, toml::Value>,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub enum Environment {
    #[default]
    Dev,
    Test,
    Prod,
}

impl Settings {
    /// Read a scalar string from the `extra` map by key. Returns
    /// `None` if the key is absent or the value isn't a string.
    ///
    /// Most app-defined settings are scalar (`UMBRA_OPENAI_API_KEY=
    /// sk-test`), so this helper is the right shape for the common
    /// case. For nested tables (`[external.openai]` in `umbra.toml`)
    /// the caller indexes into `extra` directly: `settings.extra.
    /// get("external").and_then(|v| v.get("openai")).and_then(...)`.
    pub fn extra_str(&self, key: &str) -> Option<&str> {
        self.extra.get(key).and_then(|v| v.as_str())
    }

    /// Load settings from defaults, `umbra.toml`, and `UMBRA_`-prefixed env vars.
    ///
    /// Precedence (later wins): struct defaults → `umbra.toml` → env vars.
    /// Implementation uses `merge` (not `join`) for both providers so each
    /// subsequent source overrides the previous one's values. With `join`
    /// the first provider to set a key would keep it, which would invert
    /// the documented precedence.
    ///
    /// The error type is boxed because `figment::Error` is large (over 200
    /// bytes); see `clippy::result_large_err`.
    pub fn from_env() -> Result<Self, Box<figment::Error>> {
        Figment::new()
            .merge(Toml::file("umbra.toml"))
            .merge(Env::prefixed("UMBRA_").split("__"))
            .extract()
            .map_err(Box::new)
    }
}

#[cfg(test)]
#[allow(clippy::result_large_err)]
// `Jail::expect_with` takes a closure returning `figment::Result<()>`, and
// `figment::Error` is ~208 bytes. Boxing it here would only obscure tests
// without any runtime benefit, so the lint is silenced module-wide.
mod tests {
    //! `Settings::init` and `settings::get` are intentionally out of scope here:
    //! the process-wide `OnceLock` can be set exactly once per process, which
    //! is incompatible with cargo test's parallel runner. Covering them
    //! correctly needs `serial_test` or a thread-local refactor.
    use super::*;
    use figment::Jail;

    #[test]
    fn defaults_apply_when_nothing_is_set() {
        Jail::expect_with(|_| {
            let s = Settings::from_env().unwrap();
            assert_eq!(s.database_url, "sqlite::memory:");
            assert_eq!(s.secret_key, "umbra-insecure-dev-key-change-me");
            assert_eq!(s.allowed_hosts, vec!["localhost", "127.0.0.1"]);
            assert_eq!(s.log_level, "info");
            assert!(matches!(s.environment, Environment::Dev));
            assert!(s.databases.is_empty());
            Ok(())
        });
    }

    #[test]
    fn umbra_env_var_overrides_database_url() {
        Jail::expect_with(|jail| {
            jail.set_env("UMBRA_DATABASE_URL", "postgres://example");
            let s = Settings::from_env().unwrap();
            assert_eq!(s.database_url, "postgres://example");
            Ok(())
        });
    }

    #[test]
    fn nested_env_var_populates_databases_map() {
        Jail::expect_with(|jail| {
            jail.set_env("UMBRA_DATABASES__REPLICA", "sqlite://replica.db");
            let s = Settings::from_env().unwrap();
            assert_eq!(
                s.databases.get("replica").map(String::as_str),
                Some("sqlite://replica.db"),
            );
            Ok(())
        });
    }

    #[test]
    fn umbra_toml_in_cwd_is_loaded() {
        Jail::expect_with(|jail| {
            jail.create_file("umbra.toml", r#"secret_key = "from-toml""#)?;
            let s = Settings::from_env().unwrap();
            assert_eq!(s.secret_key, "from-toml");
            Ok(())
        });
    }

    #[test]
    fn env_var_overrides_toml() {
        // Matches the precedence documented on `Settings::from_env`:
        // env vars override toml. The implementation uses `merge` (not
        // `join`) precisely so this assertion holds.
        Jail::expect_with(|jail| {
            jail.create_file("umbra.toml", r#"secret_key = "from-toml""#)?;
            jail.set_env("UMBRA_SECRET_KEY", "from-env");
            let s = Settings::from_env().unwrap();
            assert_eq!(s.secret_key, "from-env");
            Ok(())
        });
    }

    #[test]
    fn environment_default_is_dev() {
        assert!(matches!(Environment::default(), Environment::Dev));
    }

    #[test]
    fn environment_prod_round_trips_through_toml() {
        Jail::expect_with(|jail| {
            jail.create_file("umbra.toml", r#"environment = "Prod""#)?;
            let s = Settings::from_env().unwrap();
            assert!(matches!(s.environment, Environment::Prod));
            Ok(())
        });
    }

    /// An `UMBRA_`-prefixed env var that doesn't correspond to a known
    /// `Settings` field falls into `extra` so user code can read it.
    /// `OPENAI_API_KEY` stands in for the common "I have an external
    /// service credential" case.
    #[test]
    fn unknown_env_var_is_captured_in_extra() {
        Jail::expect_with(|jail| {
            jail.set_env("UMBRA_OPENAI_API_KEY", "sk-test-12345");
            let s = Settings::from_env().unwrap();
            assert_eq!(s.extra_str("openai_api_key"), Some("sk-test-12345"));
            // Known fields still resolve normally.
            assert_eq!(s.database_url, "sqlite::memory:");
            Ok(())
        });
    }

    /// A nested `umbra.toml` table that doesn't map to a known field
    /// preserves its structure inside `extra`. The accessor walks the
    /// nested table directly via `toml::Value`.
    #[test]
    fn unknown_toml_table_is_captured_in_extra() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "umbra.toml",
                r#"
                [external]
                provider = "stripe"
                "#,
            )?;
            let s = Settings::from_env().unwrap();
            let provider = s
                .extra
                .get("external")
                .and_then(|v| v.get("provider"))
                .and_then(|v| v.as_str());
            assert_eq!(provider, Some("stripe"));
            Ok(())
        });
    }
}
