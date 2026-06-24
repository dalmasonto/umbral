use figment::Figment;
use figment::providers::{Env, Format, Serialized, Toml};
use serde::Deserialize;
use std::collections::HashSet;
use std::sync::OnceLock;

/// Ambient settings, published during `AppBuilder::build()`.
pub(crate) static SETTINGS: OnceLock<Settings> = OnceLock::new();

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

/// Return the ambient settings if they have been initialised, or `None`.
///
/// Unlike [`get`], this function never panics. Useful in plugin code
/// that may run before `App::build()` (e.g. during tests or in
/// route-builder helpers that check the environment at build time).
pub fn get_opt() -> Option<&'static Settings> {
    SETTINGS.get()
}

fn default_database_url() -> String {
    // In-memory SQLite so first-run with all defaults works without any
    // filesystem assumptions (a sqlite:// URL pointing at a non-existent
    // file errors out without `?mode=rwc`). Real apps override this via
    // umbra.toml or UMBRA_DATABASE_URL.
    "sqlite::memory:".into()
}

/// Default `Form<T>` body cap: 16 MiB — generous for urlencoded forms while
/// still a DoS guard, and 8× the old hardcoded 2 MiB. Override via
/// `UMBRA_MAX_FORM_BODY_BYTES`, or set `0` to disable.
fn default_max_form_body_bytes() -> Option<usize> {
    Some(16 * 1024 * 1024)
}

fn default_secret_key() -> String {
    "umbra-insecure-dev-key-change-me".into()
}

fn default_allowed_hosts() -> Vec<String> {
    vec!["localhost".into(), "127.0.0.1".into()]
}

/// Deserialize a `Vec<String>` from either a real sequence (a TOML array, or a
/// bracketed env value like `["a.com","b.com"]`) OR a single comma-separated
/// string (`UMBRA_ALLOWED_HOSTS=a.com,b.com`). Env vars are scalar strings, so
/// without this a list-valued setting can only be set with the non-obvious
/// bracketed form — the natural `HOST1,HOST2` (Django's convention) would error
/// with "expected a sequence". Whitespace is trimmed and empty entries dropped.
fn deserialize_string_list<'de, D>(de: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    Ok(match OneOrMany::deserialize(de)? {
        OneOrMany::One(s) => s
            .split(',')
            .map(str::trim)
            .filter(|h| !h.is_empty())
            .map(str::to_string)
            .collect(),
        OneOrMany::Many(v) => v,
    })
}

fn default_log_level() -> String {
    "info".into()
}

/// PERF-5: pool size default (matches sqlx's own default of 10). Raise
/// for a high-concurrency Postgres deploy via `UMBRA_DB_MAX_CONNECTIONS`.
fn default_db_max_connections() -> u32 {
    10
}

/// PERF-5: seconds to wait for a free pooled connection before failing a
/// request. A bounded timeout means a saturated pool fails fast (503)
/// instead of blocking the request task forever.
fn default_db_acquire_timeout_secs() -> u64 {
    30
}

/// gaps2 #91: idle-connection floor. `0` means "shrink to zero idle
/// connections" — sqlx's own default. Raise it on a busy service to keep
/// warm connections ready (saves the per-request TCP+TLS+auth handshake).
fn default_db_min_connections() -> u32 {
    0
}

/// gaps2 #91: close a connection that's been idle this many seconds.
/// Default 10 minutes — reclaims connections during quiet periods so the
/// pool doesn't pin `max_connections` slots on the server forever. `None`
/// (env `0`/empty) disables idle reaping.
fn default_db_idle_timeout_secs() -> Option<u64> {
    Some(600)
}

/// gaps2 #91: recycle any connection older than this many seconds,
/// regardless of activity. Default 30 minutes — defends against stale
/// connections silently dropped by a load balancer or reaped by
/// Postgres's `idle_in_transaction_session_timeout`. `None` (env
/// `0`/empty) disables lifetime recycling.
fn default_db_max_lifetime_secs() -> Option<u64> {
    Some(1800)
}

/// gaps2 #91: health-check a pooled connection (a cheap `SELECT`/ping)
/// before handing it to a caller. Default `true` — a dead connection
/// (server restarted, network blip) is silently replaced instead of
/// surfacing as a mid-request error. Set `false` to trade safety for a
/// few microseconds per acquire on a known-stable network.
fn default_db_test_before_acquire() -> bool {
    true
}

fn default_bind_addr() -> String {
    // 127.0.0.1 only by default — exposing the server on 0.0.0.0
    // is a deliberate keystroke. Override with UMBRA_BIND_ADDR or
    // umbra.toml.
    "127.0.0.1:8000".into()
}

fn default_static_url() -> String {
    "/static/".into()
}

fn default_static_root() -> String {
    "staticfiles/".into()
}

/// Normalise a `static_url` so it always carries exactly one leading
/// and one trailing slash. `"/static"`, `"static"`, and `"/static/"`
/// all converge on `"/static/"`. A CDN-style absolute URL
/// (`"https://cdn.example.com/s"`) keeps its scheme+host and gains the
/// trailing slash (`"https://cdn.example.com/s/"`) without acquiring a
/// spurious leading slash. An empty value normalises to `"/"`.
///
/// The leading-slash rule only applies to root-relative paths; a value
/// that already starts with `http://`, `https://`, or `//` is treated
/// as absolute and left with its prefix intact.
fn normalize_static_url(raw: &str) -> String {
    let trimmed = raw.trim();
    let is_absolute = trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
        || trimmed.starts_with("//");

    let mut out = String::with_capacity(trimmed.len() + 2);
    if is_absolute {
        out.push_str(trimmed.trim_end_matches('/'));
    } else {
        out.push('/');
        out.push_str(trimmed.trim_matches('/'));
    }
    if !out.ends_with('/') {
        out.push('/');
    }
    out
}

/// Deserialize and normalise `static_url` in one step so the invariant
/// (leading + trailing slash) holds no matter the source — toml, env,
/// or the struct default. Serde applies this to the raw string before
/// it ever reaches a reader.
fn deserialize_static_url<'de, D>(de: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = String::deserialize(de)?;
    Ok(normalize_static_url(&raw))
}

/// Deserialize an `Option<u64>` where `0` (and an empty/missing string,
/// as an env var might supply) maps to `None` — the "disabled" sentinel
/// for the idle/max-lifetime timeouts (gaps2 #91). Accepts an integer
/// (toml), a numeric string (env/dotenv), or an explicit null.
fn deserialize_zero_as_none<'de, D>(de: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Int(u64),
        Str(String),
        Null,
    }

    let value = match Option::<Raw>::deserialize(de)? {
        None | Some(Raw::Null) => return Ok(None),
        Some(Raw::Int(n)) => n,
        Some(Raw::Str(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            trimmed.parse::<u64>().map_err(D::Error::custom)?
        }
    };

    Ok(if value == 0 { None } else { Some(value) })
}

fn dotenv_key(key: &str) -> Option<String> {
    const PREFIX: &str = "UMBRA_";

    let key = key.trim();
    if key.len() <= PREFIX.len() || !key.get(..PREFIX.len())?.eq_ignore_ascii_case(PREFIX) {
        return None;
    }

    let key = key[PREFIX.len()..].replace("__", ".").to_ascii_lowercase();
    if key.split('.').any(str::is_empty) {
        return None;
    }

    Some(key)
}

fn merge_dotenv(mut figment: Figment) -> Figment {
    let Ok(iter) = dotenvy::from_filename_iter(".env") else {
        return figment;
    };
    let mut seen = HashSet::new();

    for (key, value) in iter.flatten() {
        let Some(key) = dotenv_key(&key) else {
            continue;
        };
        if !seen.insert(key.clone()) {
            continue;
        }
        let value = value
            .parse::<figment::value::Value>()
            .expect("figment value parsing is infallible");
        figment = figment.merge(Serialized::default(&key, value));
    }

    figment
}

#[derive(Clone, Debug, Deserialize)]
pub struct Settings {
    #[serde(default = "default_database_url")]
    pub database_url: String,

    #[serde(default)]
    pub databases: std::collections::HashMap<String, String>,

    /// Max request-body size (bytes) the `Form<T>` extractor buffers before
    /// returning `413 Payload Too Large`. Default **16 MiB** (8× the old
    /// hardcoded 2 MiB). Set `UMBRA_MAX_FORM_BODY_BYTES` (or `max_form_body_bytes`
    /// in `umbra.toml`); set it to `0` to **disable** the cap entirely — handy
    /// in dev. (For large uploads use a file field / the storage backend, not
    /// the form extractor.)
    #[serde(default = "default_max_form_body_bytes")]
    pub max_form_body_bytes: Option<usize>,

    /// Max connections in the Postgres pool (PERF-5). Default 10. Set via
    /// `UMBRA_DB_MAX_CONNECTIONS` or `db_max_connections` in `umbra.toml`.
    #[serde(default = "default_db_max_connections")]
    pub db_max_connections: u32,

    /// Seconds to wait for a free pooled connection before failing the
    /// request (Postgres acquire timeout, PERF-5). Default 30. Set via
    /// `UMBRA_DB_ACQUIRE_TIMEOUT_SECS` or `db_acquire_timeout_secs`.
    #[serde(default = "default_db_acquire_timeout_secs")]
    pub db_acquire_timeout_secs: u64,

    /// Idle-connection floor — the pool keeps at least this many warm
    /// connections (gaps2 #91). Default 0. Set via
    /// `UMBRA_DB_MIN_CONNECTIONS` or `db_min_connections`.
    #[serde(default = "default_db_min_connections")]
    pub db_min_connections: u32,

    /// Close a connection after it's been idle this many seconds (gaps2
    /// #91). Default 600 (10 min). `0`/empty disables idle reaping. Set
    /// via `UMBRA_DB_IDLE_TIMEOUT_SECS` or `db_idle_timeout_secs`.
    #[serde(
        default = "default_db_idle_timeout_secs",
        deserialize_with = "deserialize_zero_as_none"
    )]
    pub db_idle_timeout_secs: Option<u64>,

    /// Recycle a connection older than this many seconds (gaps2 #91).
    /// Default 1800 (30 min) — avoids stale connections behind a load
    /// balancer / Postgres idle-reaping. `0`/empty disables. Set via
    /// `UMBRA_DB_MAX_LIFETIME_SECS` or `db_max_lifetime_secs`.
    #[serde(
        default = "default_db_max_lifetime_secs",
        deserialize_with = "deserialize_zero_as_none"
    )]
    pub db_max_lifetime_secs: Option<u64>,

    /// Health-check a pooled connection before handing it out (gaps2
    /// #91). Default true. Set via `UMBRA_DB_TEST_BEFORE_ACQUIRE` or
    /// `db_test_before_acquire`.
    #[serde(default = "default_db_test_before_acquire")]
    pub db_test_before_acquire: bool,

    #[serde(default = "default_secret_key")]
    pub secret_key: String,

    #[serde(default)]
    pub environment: Environment,

    #[serde(
        default = "default_allowed_hosts",
        deserialize_with = "deserialize_string_list"
    )]
    pub allowed_hosts: Vec<String>,

    #[serde(default = "default_log_level")]
    pub log_level: String,

    /// The address the development server binds to.
    /// `host:port` format, e.g. `127.0.0.1:8000` (default), `0.0.0.0:80`,
    /// `[::1]:8000`. Override with `UMBRA_BIND_ADDR` or `umbra.toml`.
    #[serde(default = "default_bind_addr")]
    pub bind_addr: String,

    /// Gap 106 — timezone for marshalling naive datetimes on the
    /// read and write boundary. `None` (default) keeps the
    /// historical UTC-everywhere behaviour: naive input is treated
    /// as UTC, admin-form display renders the stored UTC value
    /// verbatim.
    ///
    /// `Some("Africa/Nairobi")` (any IANA tz name resolvable via
    /// `chrono-tz`) flips both ends: HTML `<input type="datetime-
    /// local">` values arriving naive are interpreted in the
    /// configured tz then converted to UTC before storage; the
    /// admin form renders stored UTC values converted back to the
    /// configured tz so the user sees wall-clock time, not UTC.
    /// Column type stays `TIMESTAMPTZ` (Postgres) / `TEXT`
    /// (SQLite) — only the marshalling layer changes.
    ///
    /// Set via `UMBRA_TIME_ZONE=Africa/Nairobi` or
    /// `time_zone = "Africa/Nairobi"` in `umbra.toml`. An unknown
    /// tz name falls back to UTC at lookup time with a tracing
    /// warning rather than panicking — startup never fails on a
    /// tz config error.
    #[serde(default)]
    pub time_zone: Option<String>,

    /// URL prefix every collected/served static asset hangs under.
    ///
    /// Default `"/static/"`. The framework's static handler mounts at
    /// this base and the `static()` template helper prepends it, so
    /// `{{ static("admin/admin.css") }}` resolves to
    /// `"/static/admin/admin.css"`. Set a CDN origin
    /// (`UMBRA_STATIC_URL=https://cdn.example.com/s/`) to serve assets
    /// off a separate host in production — the helper then emits
    /// absolute URLs and the local handler simply goes unused.
    ///
    /// Always normalised to carry exactly one leading and one trailing
    /// slash: `"/static"`, `"static"`, and `"/static/"` all converge on
    /// `"/static/"`. Set via `UMBRA_STATIC_URL` or `static_url` in
    /// `umbra.toml`.
    #[serde(
        default = "default_static_url",
        deserialize_with = "deserialize_static_url"
    )]
    pub static_url: String,

    /// On-disk directory collected static assets live under in
    /// production.
    ///
    /// Default `"staticfiles/"` (relative to the binary's CWD). The
    /// static handler resolves a request `/static/<ns>/<rest>` to
    /// `<static_root>/<ns>/<rest>` in prod, and as the dev fallback
    /// when a plugin's live source dir doesn't have the file. Set via
    /// `UMBRA_STATIC_ROOT` or `static_root` in `umbra.toml`.
    #[serde(default = "default_static_root")]
    pub static_root: String,

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

    /// Load settings from defaults, `.env`, `umbra.toml`, and `UMBRA_`-prefixed env vars.
    ///
    /// Precedence (later wins): struct defaults → `umbra.toml` → env vars. A
    /// local `.env` file is merged as an environment-shaped provider first,
    /// but existing process env vars keep precedence over values from `.env`.
    /// Implementation uses `merge` (not `join`) for both providers so each
    /// subsequent source overrides the previous one's values. With `join`
    /// the first provider to set a key would keep it, which would invert
    /// the documented precedence.
    ///
    /// The error type is boxed because `figment::Error` is large (over 200
    /// bytes); see `clippy::result_large_err`.
    pub fn from_env() -> Result<Self, Box<figment::Error>> {
        merge_dotenv(Figment::new().merge(Toml::file("umbra.toml")))
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
    fn allowed_hosts_accepts_comma_separated_env() {
        // The Django-style natural form: `UMBRA_ALLOWED_HOSTS=a.com,b.com`.
        Jail::expect_with(|jail| {
            jail.set_env("UMBRA_ALLOWED_HOSTS", "example.com, www.example.com");
            let s = Settings::from_env().unwrap();
            assert_eq!(s.allowed_hosts, vec!["example.com", "www.example.com"]);
            Ok(())
        });
    }

    #[test]
    fn allowed_hosts_accepts_single_env_value() {
        Jail::expect_with(|jail| {
            jail.set_env("UMBRA_ALLOWED_HOSTS", "example.com");
            let s = Settings::from_env().unwrap();
            assert_eq!(s.allowed_hosts, vec!["example.com"]);
            Ok(())
        });
    }

    #[test]
    fn allowed_hosts_accepts_bracketed_env_and_toml_array() {
        Jail::expect_with(|jail| {
            jail.set_env("UMBRA_ALLOWED_HOSTS", r#"["a.com","b.com"]"#);
            assert_eq!(
                Settings::from_env().unwrap().allowed_hosts,
                vec!["a.com", "b.com"]
            );
            Ok(())
        });
        Jail::expect_with(|jail| {
            jail.create_file("umbra.toml", r#"allowed_hosts = ["a.com", "b.com"]"#)?;
            assert_eq!(
                Settings::from_env().unwrap().allowed_hosts,
                vec!["a.com", "b.com"]
            );
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
    fn dotenv_file_overrides_toml() {
        Jail::expect_with(|jail| {
            jail.create_file("umbra.toml", r#"database_url = "sqlite://from-toml.db""#)?;
            jail.create_file(".env", "UMBRA_DATABASE_URL=postgres://from-dotenv\n")?;
            let s = Settings::from_env().unwrap();
            assert_eq!(s.database_url, "postgres://from-dotenv");
            Ok(())
        });
    }

    #[test]
    fn dotenv_file_populates_nested_databases_map() {
        Jail::expect_with(|jail| {
            jail.create_file(".env", "UMBRA_DATABASES__REPLICA=sqlite://replica.db\n")?;
            let s = Settings::from_env().unwrap();
            assert_eq!(
                s.databases.get("replica").map(String::as_str),
                Some("sqlite://replica.db"),
            );
            Ok(())
        });
    }

    #[test]
    fn process_env_overrides_dotenv_file() {
        Jail::expect_with(|jail| {
            jail.create_file(".env", "UMBRA_DATABASE_URL=postgres://from-dotenv\n")?;
            jail.set_env("UMBRA_DATABASE_URL", "postgres://from-process-env");
            let s = Settings::from_env().unwrap();
            assert_eq!(s.database_url, "postgres://from-process-env");
            Ok(())
        });
    }

    #[test]
    fn static_url_and_root_defaults() {
        Jail::expect_with(|_| {
            let s = Settings::from_env().unwrap();
            assert_eq!(s.static_url, "/static/");
            assert_eq!(s.static_root, "staticfiles/");
            Ok(())
        });
    }

    #[test]
    fn static_url_env_override_is_normalised() {
        // No trailing slash on input -> normalised to one.
        Jail::expect_with(|jail| {
            jail.set_env("UMBRA_STATIC_URL", "/assets");
            assert_eq!(Settings::from_env().unwrap().static_url, "/assets/");
            Ok(())
        });
        // No leading slash either.
        Jail::expect_with(|jail| {
            jail.set_env("UMBRA_STATIC_URL", "assets");
            assert_eq!(Settings::from_env().unwrap().static_url, "/assets/");
            Ok(())
        });
        // Already-normalised value is left intact.
        Jail::expect_with(|jail| {
            jail.set_env("UMBRA_STATIC_URL", "/assets/");
            assert_eq!(Settings::from_env().unwrap().static_url, "/assets/");
            Ok(())
        });
    }

    #[test]
    fn static_url_normalises_three_input_shapes() {
        // The three canonical shapes from the spec all converge.
        assert_eq!(normalize_static_url("/static"), "/static/");
        assert_eq!(normalize_static_url("static"), "/static/");
        assert_eq!(normalize_static_url("/static/"), "/static/");
    }

    #[test]
    fn static_url_cdn_origin_keeps_scheme_and_host() {
        // An absolute CDN URL keeps its scheme+host and only gains a
        // trailing slash — no spurious leading slash collapsing `https://`.
        assert_eq!(
            normalize_static_url("https://cdn.example.com/s"),
            "https://cdn.example.com/s/"
        );
        assert_eq!(
            normalize_static_url("https://cdn.example.com/s/"),
            "https://cdn.example.com/s/"
        );
    }

    #[test]
    fn static_root_env_override() {
        Jail::expect_with(|jail| {
            jail.set_env("UMBRA_STATIC_ROOT", "build/assets/");
            assert_eq!(Settings::from_env().unwrap().static_root, "build/assets/");
            Ok(())
        });
    }

    #[test]
    fn db_pool_defaults_apply_when_nothing_is_set() {
        Jail::expect_with(|_| {
            let s = Settings::from_env().unwrap();
            assert_eq!(s.db_max_connections, 10);
            assert_eq!(s.db_min_connections, 0);
            assert_eq!(s.db_acquire_timeout_secs, 30);
            assert_eq!(s.db_idle_timeout_secs, Some(600));
            assert_eq!(s.db_max_lifetime_secs, Some(1800));
            assert!(s.db_test_before_acquire);
            Ok(())
        });
    }

    #[test]
    fn db_pool_env_overrides_each_knob() {
        Jail::expect_with(|jail| {
            jail.set_env("UMBRA_DB_MAX_CONNECTIONS", "42");
            jail.set_env("UMBRA_DB_MIN_CONNECTIONS", "4");
            jail.set_env("UMBRA_DB_ACQUIRE_TIMEOUT_SECS", "7");
            jail.set_env("UMBRA_DB_IDLE_TIMEOUT_SECS", "120");
            jail.set_env("UMBRA_DB_MAX_LIFETIME_SECS", "240");
            jail.set_env("UMBRA_DB_TEST_BEFORE_ACQUIRE", "false");
            let s = Settings::from_env().unwrap();
            assert_eq!(s.db_max_connections, 42);
            assert_eq!(s.db_min_connections, 4);
            assert_eq!(s.db_acquire_timeout_secs, 7);
            assert_eq!(s.db_idle_timeout_secs, Some(120));
            assert_eq!(s.db_max_lifetime_secs, Some(240));
            assert!(!s.db_test_before_acquire);
            Ok(())
        });
    }

    #[test]
    fn db_timeout_zero_means_disabled_none() {
        Jail::expect_with(|jail| {
            jail.set_env("UMBRA_DB_IDLE_TIMEOUT_SECS", "0");
            jail.set_env("UMBRA_DB_MAX_LIFETIME_SECS", "0");
            let s = Settings::from_env().unwrap();
            assert_eq!(s.db_idle_timeout_secs, None);
            assert_eq!(s.db_max_lifetime_secs, None);
            Ok(())
        });
    }

    #[test]
    fn db_timeout_empty_string_means_disabled_none() {
        Jail::expect_with(|jail| {
            jail.set_env("UMBRA_DB_IDLE_TIMEOUT_SECS", "");
            let s = Settings::from_env().unwrap();
            assert_eq!(s.db_idle_timeout_secs, None);
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
