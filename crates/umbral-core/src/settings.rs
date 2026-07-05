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
        .expect("umbral::settings::init called more than once");
}

/// Return a reference to the ambient settings.
///
/// # Panics
///
/// Panics if `App::build()` hasn't run.
pub fn get() -> &'static Settings {
    SETTINGS
        .get()
        .expect("umbral: settings not initialised — did you call App::build()?")
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
    // umbral.toml or UMBRAL_DATABASE_URL.
    "sqlite::memory:".into()
}

/// Default `Form<T>` body cap: 16 MiB — generous for urlencoded forms while
/// still a DoS guard, and 8× the old hardcoded 2 MiB. Override via
/// `UMBRAL_MAX_FORM_BODY_BYTES`, or set `0` to disable.
fn default_max_form_body_bytes() -> Option<usize> {
    Some(16 * 1024 * 1024)
}

fn default_secret_key() -> String {
    "umbral-insecure-dev-key-change-me".into()
}

fn default_allowed_hosts() -> Vec<String> {
    vec!["localhost".into(), "127.0.0.1".into()]
}

/// Deserialize a `Vec<String>` from either a real sequence (a TOML array, or a
/// bracketed env value like `["a.com","b.com"]`) OR a single comma-separated
/// string (`UMBRAL_ALLOWED_HOSTS=a.com,b.com`). Env vars are scalar strings, so
/// without this a list-valued setting can only be set with the non-obvious
/// bracketed form; the natural `HOST1,HOST2` comma-separated form would error
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
/// for a high-concurrency Postgres deploy via `UMBRAL_DB_MAX_CONNECTIONS`.
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
    // is a deliberate keystroke. Override with UMBRAL_BIND_ADDR or
    // umbral.toml.
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

/// Case-insensitive `Environment` deserialization (audit_2 core-app-config
/// #16). The variants are `Dev` / `Test` / `Prod`, but every operator hint
/// aside, `UMBRAL_ENVIRONMENT=prod` (lowercase — the natural thing to type)
/// otherwise fails deserialization with a generic figment variant error.
/// Accept any case plus the common long forms so a lowercase value boots the
/// intended environment instead of erroring.
fn deserialize_environment<'de, D>(de: D) -> Result<Environment, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;
    let raw = String::deserialize(de)?;
    match raw.trim().to_ascii_lowercase().as_str() {
        "dev" | "development" => Ok(Environment::Dev),
        "test" | "testing" => Ok(Environment::Test),
        "prod" | "production" => Ok(Environment::Prod),
        other => Err(D::Error::custom(format!(
            "unknown environment `{other}`; expected one of Dev, Test, Prod (case-insensitive)"
        ))),
    }
}

fn dotenv_key(key: &str) -> Option<String> {
    const PREFIX: &str = "UMBRAL_";

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

#[derive(Clone, Deserialize)]
pub struct Settings {
    #[serde(default = "default_database_url")]
    pub database_url: String,

    #[serde(default)]
    pub databases: std::collections::HashMap<String, String>,

    /// Max request-body size (bytes) the `Form<T>` extractor buffers before
    /// returning `413 Payload Too Large`. Default **16 MiB** (8× the old
    /// hardcoded 2 MiB). Set `UMBRAL_MAX_FORM_BODY_BYTES` (or `max_form_body_bytes`
    /// in `umbral.toml`); set it to `0` to **disable** the cap entirely — handy
    /// in dev. (For large uploads use a file field / the storage backend, not
    /// the form extractor.)
    #[serde(default = "default_max_form_body_bytes")]
    pub max_form_body_bytes: Option<usize>,

    /// Max connections in the Postgres pool (PERF-5). Default 10. Set via
    /// `UMBRAL_DB_MAX_CONNECTIONS` or `db_max_connections` in `umbral.toml`.
    #[serde(default = "default_db_max_connections")]
    pub db_max_connections: u32,

    /// Seconds to wait for a free pooled connection before failing the
    /// request (Postgres acquire timeout, PERF-5). Default 30. Set via
    /// `UMBRAL_DB_ACQUIRE_TIMEOUT_SECS` or `db_acquire_timeout_secs`.
    #[serde(default = "default_db_acquire_timeout_secs")]
    pub db_acquire_timeout_secs: u64,

    /// Idle-connection floor — the pool keeps at least this many warm
    /// connections (gaps2 #91). Default 0. Set via
    /// `UMBRAL_DB_MIN_CONNECTIONS` or `db_min_connections`.
    #[serde(default = "default_db_min_connections")]
    pub db_min_connections: u32,

    /// Close a connection after it's been idle this many seconds (gaps2
    /// #91). Default 600 (10 min). `0`/empty disables idle reaping. Set
    /// via `UMBRAL_DB_IDLE_TIMEOUT_SECS` or `db_idle_timeout_secs`.
    #[serde(
        default = "default_db_idle_timeout_secs",
        deserialize_with = "deserialize_zero_as_none"
    )]
    pub db_idle_timeout_secs: Option<u64>,

    /// Recycle a connection older than this many seconds (gaps2 #91).
    /// Default 1800 (30 min) — avoids stale connections behind a load
    /// balancer / Postgres idle-reaping. `0`/empty disables. Set via
    /// `UMBRAL_DB_MAX_LIFETIME_SECS` or `db_max_lifetime_secs`.
    #[serde(
        default = "default_db_max_lifetime_secs",
        deserialize_with = "deserialize_zero_as_none"
    )]
    pub db_max_lifetime_secs: Option<u64>,

    /// Health-check a pooled connection before handing it out (gaps2
    /// #91). Default true. Set via `UMBRAL_DB_TEST_BEFORE_ACQUIRE` or
    /// `db_test_before_acquire`.
    #[serde(default = "default_db_test_before_acquire")]
    pub db_test_before_acquire: bool,

    #[serde(default = "default_secret_key")]
    pub secret_key: String,

    #[serde(default, deserialize_with = "deserialize_environment")]
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
    /// `[::1]:8000`. Override with `UMBRAL_BIND_ADDR` or `umbral.toml`.
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
    /// Set via `UMBRAL_TIME_ZONE=Africa/Nairobi` or
    /// `time_zone = "Africa/Nairobi"` in `umbral.toml`. An unknown
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
    /// (`UMBRAL_STATIC_URL=https://cdn.example.com/s/`) to serve assets
    /// off a separate host in production — the helper then emits
    /// absolute URLs and the local handler simply goes unused.
    ///
    /// Always normalised to carry exactly one leading and one trailing
    /// slash: `"/static"`, `"static"`, and `"/static/"` all converge on
    /// `"/static/"`. Set via `UMBRAL_STATIC_URL` or `static_url` in
    /// `umbral.toml`.
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
    /// `UMBRAL_STATIC_ROOT` or `static_root` in `umbral.toml`.
    #[serde(default = "default_static_root")]
    pub static_root: String,

    /// Catch-all for `UMBRAL_`-prefixed environment variables (and
    /// `umbral.toml` keys) that don't map to a named field above.
    ///
    /// Real apps usually need keys the framework doesn't know about —
    /// `OPENAI_API_KEY`, `STRIPE_SECRET`, third-party plugin
    /// configuration. Setting `UMBRAL_OPENAI_API_KEY=sk-test` makes
    /// `settings.extra.get("openai_api_key")` return a string value
    /// without the user crate having to wire a second figment loader.
    ///
    /// Values are stored as `toml::Value` so a nested
    /// `[external.openai]` table in `umbral.toml` round-trips with its
    /// structure intact. The accessor [`Settings::extra_str`] handles
    /// the common scalar-string case.
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, toml::Value>,
}

/// Redact the userinfo (`user:password`) of a connection URL, keeping the
/// scheme and host so the value stays diagnosable without leaking the
/// password. `postgres://alice:s3cret@db.host/app` →
/// `postgres://***@db.host/app`. A URL with no `@` (e.g. `sqlite::memory:`)
/// is returned unchanged.
fn redact_url_userinfo(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_string();
    };
    let after = scheme_end + 3;
    // Only treat an `@` in the authority section (before the first `/`,
    // `?`, or `#`) as a userinfo delimiter.
    let authority_end = url[after..]
        .find(['/', '?', '#'])
        .map(|i| after + i)
        .unwrap_or(url.len());
    match url[after..authority_end].find('@') {
        Some(at) => format!("{}***{}", &url[..after], &url[after + at..]),
        None => url.to_string(),
    }
}

/// Newtype so the redacting `Debug` for [`Settings`] can print the
/// `databases` map with each URL's userinfo masked.
struct RedactedDatabases<'a>(&'a std::collections::HashMap<String, String>);

impl std::fmt::Debug for RedactedDatabases<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map()
            .entries(self.0.iter().map(|(k, v)| (k, redact_url_userinfo(v))))
            .finish()
    }
}

/// Newtype so the redacting `Debug` for [`Settings`] can print the `extra`
/// map's keys (useful for spotting a typo'd setting) while masking every
/// value — `extra` is where arbitrary third-party API keys land.
struct RedactedExtra<'a>(&'a std::collections::HashMap<String, toml::Value>);

impl std::fmt::Debug for RedactedExtra<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map()
            .entries(self.0.keys().map(|k| (k, "***")))
            .finish()
    }
}

/// Hand-written, redacting `Debug` (audit_2 core-app-config #11). The derived
/// `Debug` printed `secret_key`, the DB password inside `database_url` /
/// `databases`, and every `extra` value in plaintext — one `tracing::debug!
/// (?settings)` or `?ctx` (which embeds `Settings`) away from leaking every
/// credential the app holds. This impl masks the three secret-bearing fields
/// and prints the rest verbatim so the value stays useful for debugging.
impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Settings")
            .field("database_url", &redact_url_userinfo(&self.database_url))
            .field("databases", &RedactedDatabases(&self.databases))
            .field("max_form_body_bytes", &self.max_form_body_bytes)
            .field("db_max_connections", &self.db_max_connections)
            .field("db_acquire_timeout_secs", &self.db_acquire_timeout_secs)
            .field("db_min_connections", &self.db_min_connections)
            .field("db_idle_timeout_secs", &self.db_idle_timeout_secs)
            .field("db_max_lifetime_secs", &self.db_max_lifetime_secs)
            .field("db_test_before_acquire", &self.db_test_before_acquire)
            .field("secret_key", &"***redacted***")
            .field("environment", &self.environment)
            .field("allowed_hosts", &self.allowed_hosts)
            .field("log_level", &self.log_level)
            .field("bind_addr", &self.bind_addr)
            .field("time_zone", &self.time_zone)
            .field("static_url", &self.static_url)
            .field("static_root", &self.static_root)
            .field("extra", &RedactedExtra(&self.extra))
            .finish()
    }
}

#[derive(Clone, Debug, Deserialize)]
pub enum Environment {
    Dev,
    Test,
    Prod,
}

impl Default for Environment {
    /// audit_2 H14 — secure by default. A **release** binary defaults to
    /// `Prod` (Host validation on, the dev `SECRET_KEY` rejected at boot, prod
    /// error pages) so a deploy that forgets to set `UMBRAL_ENVIRONMENT` is
    /// locked down instead of silently serving with the dev protections off.
    /// **Debug** builds (`cargo run`, `cargo test`) stay `Dev` for a
    /// frictionless local loop. An explicit `UMBRAL_ENVIRONMENT` always wins —
    /// this default only applies when the variable is unset (via
    /// `#[serde(default)]` on `Settings.environment`).
    fn default() -> Self {
        if cfg!(debug_assertions) {
            Environment::Dev
        } else {
            Environment::Prod
        }
    }
}

impl Settings {
    /// Read a scalar string from the `extra` map by key. Returns
    /// `None` if the key is absent or the value isn't a string.
    ///
    /// Most app-defined settings are scalar (`UMBRAL_OPENAI_API_KEY=
    /// sk-test`), so this helper is the right shape for the common
    /// case. For nested tables (`[external.openai]` in `umbral.toml`)
    /// the caller indexes into `extra` directly: `settings.extra.
    /// get("external").and_then(|v| v.get("openai")).and_then(...)`.
    pub fn extra_str(&self, key: &str) -> Option<&str> {
        self.extra.get(key).and_then(|v| v.as_str())
    }

    /// Load settings from defaults, `.env`, `umbral.toml`, and `UMBRAL_`-prefixed env vars.
    ///
    /// Precedence (later wins): struct defaults → `umbral.toml` → env vars. A
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
        merge_dotenv(Figment::new().merge(Toml::file("umbral.toml")))
            .merge(Env::prefixed("UMBRAL_").split("__"))
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
            assert_eq!(s.secret_key, "umbral-insecure-dev-key-change-me");
            assert_eq!(s.allowed_hosts, vec!["localhost", "127.0.0.1"]);
            assert_eq!(s.log_level, "info");
            assert!(matches!(s.environment, Environment::Dev));
            assert!(s.databases.is_empty());
            Ok(())
        });
    }

    #[test]
    fn allowed_hosts_accepts_comma_separated_env() {
        // The natural comma-separated form: `UMBRAL_ALLOWED_HOSTS=a.com,b.com`.
        Jail::expect_with(|jail| {
            jail.set_env("UMBRAL_ALLOWED_HOSTS", "example.com, www.example.com");
            let s = Settings::from_env().unwrap();
            assert_eq!(s.allowed_hosts, vec!["example.com", "www.example.com"]);
            Ok(())
        });
    }

    #[test]
    fn allowed_hosts_accepts_single_env_value() {
        Jail::expect_with(|jail| {
            jail.set_env("UMBRAL_ALLOWED_HOSTS", "example.com");
            let s = Settings::from_env().unwrap();
            assert_eq!(s.allowed_hosts, vec!["example.com"]);
            Ok(())
        });
    }

    #[test]
    fn allowed_hosts_accepts_bracketed_env_and_toml_array() {
        Jail::expect_with(|jail| {
            jail.set_env("UMBRAL_ALLOWED_HOSTS", r#"["a.com","b.com"]"#);
            assert_eq!(
                Settings::from_env().unwrap().allowed_hosts,
                vec!["a.com", "b.com"]
            );
            Ok(())
        });
        Jail::expect_with(|jail| {
            jail.create_file("umbral.toml", r#"allowed_hosts = ["a.com", "b.com"]"#)?;
            assert_eq!(
                Settings::from_env().unwrap().allowed_hosts,
                vec!["a.com", "b.com"]
            );
            Ok(())
        });
    }

    #[test]
    fn umbral_env_var_overrides_database_url() {
        Jail::expect_with(|jail| {
            jail.set_env("UMBRAL_DATABASE_URL", "postgres://example");
            let s = Settings::from_env().unwrap();
            assert_eq!(s.database_url, "postgres://example");
            Ok(())
        });
    }

    #[test]
    fn nested_env_var_populates_databases_map() {
        Jail::expect_with(|jail| {
            jail.set_env("UMBRAL_DATABASES__REPLICA", "sqlite://replica.db");
            let s = Settings::from_env().unwrap();
            assert_eq!(
                s.databases.get("replica").map(String::as_str),
                Some("sqlite://replica.db"),
            );
            Ok(())
        });
    }

    #[test]
    fn umbral_toml_in_cwd_is_loaded() {
        Jail::expect_with(|jail| {
            jail.create_file("umbral.toml", r#"secret_key = "from-toml""#)?;
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
            jail.create_file("umbral.toml", r#"secret_key = "from-toml""#)?;
            jail.set_env("UMBRAL_SECRET_KEY", "from-env");
            let s = Settings::from_env().unwrap();
            assert_eq!(s.secret_key, "from-env");
            Ok(())
        });
    }

    #[test]
    fn dotenv_file_overrides_toml() {
        Jail::expect_with(|jail| {
            jail.create_file("umbral.toml", r#"database_url = "sqlite://from-toml.db""#)?;
            jail.create_file(".env", "UMBRAL_DATABASE_URL=postgres://from-dotenv\n")?;
            let s = Settings::from_env().unwrap();
            assert_eq!(s.database_url, "postgres://from-dotenv");
            Ok(())
        });
    }

    #[test]
    fn dotenv_file_populates_nested_databases_map() {
        Jail::expect_with(|jail| {
            jail.create_file(".env", "UMBRAL_DATABASES__REPLICA=sqlite://replica.db\n")?;
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
            jail.create_file(".env", "UMBRAL_DATABASE_URL=postgres://from-dotenv\n")?;
            jail.set_env("UMBRAL_DATABASE_URL", "postgres://from-process-env");
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
            jail.set_env("UMBRAL_STATIC_URL", "/assets");
            assert_eq!(Settings::from_env().unwrap().static_url, "/assets/");
            Ok(())
        });
        // No leading slash either.
        Jail::expect_with(|jail| {
            jail.set_env("UMBRAL_STATIC_URL", "assets");
            assert_eq!(Settings::from_env().unwrap().static_url, "/assets/");
            Ok(())
        });
        // Already-normalised value is left intact.
        Jail::expect_with(|jail| {
            jail.set_env("UMBRAL_STATIC_URL", "/assets/");
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
            jail.set_env("UMBRAL_STATIC_ROOT", "build/assets/");
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
            jail.set_env("UMBRAL_DB_MAX_CONNECTIONS", "42");
            jail.set_env("UMBRAL_DB_MIN_CONNECTIONS", "4");
            jail.set_env("UMBRAL_DB_ACQUIRE_TIMEOUT_SECS", "7");
            jail.set_env("UMBRAL_DB_IDLE_TIMEOUT_SECS", "120");
            jail.set_env("UMBRAL_DB_MAX_LIFETIME_SECS", "240");
            jail.set_env("UMBRAL_DB_TEST_BEFORE_ACQUIRE", "false");
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
            jail.set_env("UMBRAL_DB_IDLE_TIMEOUT_SECS", "0");
            jail.set_env("UMBRAL_DB_MAX_LIFETIME_SECS", "0");
            let s = Settings::from_env().unwrap();
            assert_eq!(s.db_idle_timeout_secs, None);
            assert_eq!(s.db_max_lifetime_secs, None);
            Ok(())
        });
    }

    #[test]
    fn db_timeout_empty_string_means_disabled_none() {
        Jail::expect_with(|jail| {
            jail.set_env("UMBRAL_DB_IDLE_TIMEOUT_SECS", "");
            let s = Settings::from_env().unwrap();
            assert_eq!(s.db_idle_timeout_secs, None);
            Ok(())
        });
    }

    #[test]
    fn environment_default_is_profile_aware() {
        // audit_2 H14: debug builds default to Dev, release builds to Prod.
        // This test is correct in BOTH profiles (`cargo test` and
        // `cargo test --release`), so it pins the release branch too.
        let d = Environment::default();
        if cfg!(debug_assertions) {
            assert!(
                matches!(d, Environment::Dev),
                "debug build must default to Dev"
            );
        } else {
            assert!(
                matches!(d, Environment::Prod),
                "release build must default to Prod (H14 secure-by-default)"
            );
        }
    }

    #[test]
    fn environment_prod_round_trips_through_toml() {
        Jail::expect_with(|jail| {
            jail.create_file("umbral.toml", r#"environment = "Prod""#)?;
            let s = Settings::from_env().unwrap();
            assert!(matches!(s.environment, Environment::Prod));
            Ok(())
        });
    }

    #[test]
    fn environment_is_case_insensitive() {
        // audit_2 #16: lowercase `prod` (the natural thing to type) used to
        // fail deserialization; now it resolves to Environment::Prod.
        for value in ["prod", "PROD", "Production", "production"] {
            Jail::expect_with(|jail| {
                jail.set_env("UMBRAL_ENVIRONMENT", value);
                let s = Settings::from_env().unwrap();
                assert!(
                    matches!(s.environment, Environment::Prod),
                    "`{value}` should deserialize to Prod",
                );
                Ok(())
            });
        }
        Jail::expect_with(|jail| {
            jail.set_env("UMBRAL_ENVIRONMENT", "test");
            assert!(matches!(
                Settings::from_env().unwrap().environment,
                Environment::Test
            ));
            Ok(())
        });
    }

    #[test]
    fn environment_rejects_unknown_value() {
        Jail::expect_with(|jail| {
            jail.set_env("UMBRAL_ENVIRONMENT", "staging");
            assert!(
                Settings::from_env().is_err(),
                "an unknown environment must still be a load error"
            );
            Ok(())
        });
    }

    /// audit_2 #11: the redacting `Debug` must never surface `secret_key`,
    /// the DB password in `database_url`/`databases`, or any `extra` value.
    #[test]
    fn debug_redacts_secrets() {
        let mut databases = std::collections::HashMap::new();
        databases.insert(
            "replica".to_string(),
            "postgres://ruser:rpass@replica.host/app".to_string(),
        );
        let mut extra = std::collections::HashMap::new();
        extra.insert(
            "stripe_secret".to_string(),
            toml::Value::String("sk_live_TOPSECRET".to_string()),
        );
        let settings = Settings {
            database_url: "postgres://alice:hunter2@db.host:5432/app".to_string(),
            databases,
            max_form_body_bytes: Some(1024),
            db_max_connections: 10,
            db_acquire_timeout_secs: 30,
            db_min_connections: 0,
            db_idle_timeout_secs: Some(600),
            db_max_lifetime_secs: Some(1800),
            db_test_before_acquire: true,
            secret_key: "SUPERSECRETKEYVALUE-do-not-leak".to_string(),
            environment: Environment::Prod,
            allowed_hosts: vec!["example.com".to_string()],
            log_level: "info".to_string(),
            bind_addr: "127.0.0.1:8000".to_string(),
            time_zone: None,
            static_url: "/static/".to_string(),
            static_root: "staticfiles/".to_string(),
            extra,
        };
        let rendered = format!("{settings:?}");
        assert!(
            !rendered.contains("SUPERSECRETKEYVALUE"),
            "secret_key leaked: {rendered}"
        );
        assert!(
            !rendered.contains("hunter2"),
            "database_url password leaked: {rendered}"
        );
        assert!(
            !rendered.contains("rpass"),
            "databases password leaked: {rendered}"
        );
        assert!(
            !rendered.contains("sk_live_TOPSECRET"),
            "extra value leaked: {rendered}"
        );
        // Non-secret context is still present + useful.
        assert!(
            rendered.contains("db.host"),
            "host should survive redaction"
        );
        assert!(
            rendered.contains("stripe_secret"),
            "extra keys stay visible to spot typos"
        );
    }

    #[test]
    fn redact_url_userinfo_masks_password_keeps_host() {
        assert_eq!(
            redact_url_userinfo("postgres://alice:hunter2@db.host/app"),
            "postgres://***@db.host/app"
        );
        // No userinfo → unchanged.
        assert_eq!(redact_url_userinfo("sqlite::memory:"), "sqlite::memory:");
        assert_eq!(
            redact_url_userinfo("sqlite://data/app.db"),
            "sqlite://data/app.db"
        );
    }

    /// An `UMBRAL_`-prefixed env var that doesn't correspond to a known
    /// `Settings` field falls into `extra` so user code can read it.
    /// `OPENAI_API_KEY` stands in for the common "I have an external
    /// service credential" case.
    #[test]
    fn unknown_env_var_is_captured_in_extra() {
        Jail::expect_with(|jail| {
            jail.set_env("UMBRAL_OPENAI_API_KEY", "sk-test-12345");
            let s = Settings::from_env().unwrap();
            assert_eq!(s.extra_str("openai_api_key"), Some("sk-test-12345"));
            // Known fields still resolve normally.
            assert_eq!(s.database_url, "sqlite::memory:");
            Ok(())
        });
    }

    /// A nested `umbral.toml` table that doesn't map to a known field
    /// preserves its structure inside `extra`. The accessor walks the
    /// nested table directly via `toml::Value`.
    #[test]
    fn unknown_toml_table_is_captured_in_extra() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "umbral.toml",
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
