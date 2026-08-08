# Secrets management and abuse controls (design)

Status: draft (planning/gaps5.md #18 tf#231, and #19 tf#232)
Date: 2026-08-08
Realizes Stage 2 (self-hosted platform posture) from `docs/decisions/2026-08-08-product-north-star.md`, and feeds the boot checks of `docs/decisions/2026-08-08-enterprise-preset-design.md`.

Two operational-security features are specified here. They are related (both harden a production deployment) but ship independently:

- **#18 Secrets management.** A provider trait so `secret_key`, database URLs, and third-party keys can resolve from a KMS / Vault / cloud secrets manager instead of only from env, with rotation and metadata, plus a boot check for a default or stale `secret_key`.
- **#19 Abuse controls.** An `umbral-abuse` plugin that composes the existing throttles with captcha adapters, IP allow / deny lists, honeypots, and event-based lockout, driven by a small firewall rule config.

Both are designed to be plugins with the smallest possible core seam. Where a pure plugin cannot honestly do the job (secrets resolution), the spec says so and names the exact seam.

---

## Part 1: Secrets management (#18)

### What exists today

Settings is a single `Settings` struct in `crates/umbral-core/src/settings.rs`, deserialized by Figment from `umbral.toml` merged with `.env` and `UMBRAL_`-prefixed environment variables (`fn load`, `merge_dotenv`, `dotenv_key`). It is installed once into a process-wide `OnceLock` (`pub(crate) static SETTINGS`) at `App::build()` time and read ambiently via `umbral::settings::get()` / `get_opt()`.

The secret-bearing fields, exactly as they exist:

- `pub secret_key: String` with `#[serde(default = "default_secret_key")]`. The default is the literal `"umbral-insecure-dev-key-change-me"` (`fn default_secret_key`, line 55). It is set via `UMBRAL_SECRET_KEY` or `secret_key` in `umbral.toml`. It keys HMAC-signed CSRF tokens (see `plugins/umbral-security/src/lib.rs`, `sign()` / `CsrfState::resolve_secret`), and by convention any other signing the app does.
- `pub database_url: String` and `pub databases: HashMap<String, String>`, connection URLs which embed passwords. The redacting `Debug` impl (`RedactedDatabases`, `redact_url_userinfo`) masks userinfo so a `tracing::debug!` of `Settings` does not leak them.
- `pub extra: std::collections::HashMap<String, toml::Value>` with `#[serde(flatten)]` (line 502). This is the catch-all: any `UMBRAL_`-prefixed var (or `umbral.toml` key) that does not map to a named field lands here, so `UMBRAL_OPENAI_API_KEY=sk-...` is readable as `settings.extra_str("openai_api_key")` (accessor at line 622). The redacting `Debug` prints `extra` via `RedactedExtra` because it is where arbitrary third-party API keys land.

So today a secret is a plaintext string that Figment loaded from env / dotenv / toml. There is no KMS / Vault / Secrets Manager integration, no rotation, no per-secret metadata (version, created-at, next-rotation), and the only guard on a default key is the CSRF plugin refusing to boot in `Environment::Prod` when `secret_key` is *empty* (`check_secret_key`). Notably, `check_secret_key` does NOT catch the *default* insecure key, only the empty string. That is a real gap this design closes.

### The honest core-seam question

Secrets management cannot be a pure plugin, and pretending otherwise would be the "fix, don't patch" anti-pattern. The reason is ordering: `secret_key` and `database_url` must be resolved *before* the DB pool is built and before any plugin's `on_ready` runs. Plugins are registered on the builder and their hooks fire during / after `App::build()`; a plugin cannot retroactively supply the value that `App::build()` already consumed to open the pool. So resolution has to happen inside the settings-load path in `umbral-core`.

The seam is deliberately narrow: **core defines the `SecretsProvider` trait and one resolution pass; the backends (AWS, GCP, Azure, Vault) are plugins / optional crates.** Core never names a concrete backend, exactly as it never names a concrete `Plugin`. This mirrors the existing rule that `umbral-core` defines the `Plugin` trait but only ever touches `Box<dyn Plugin>`.

### The `SecretsProvider` trait (core)

Lives in `crates/umbral-core/src/secrets.rs`, re-exported as `umbral::secrets` (power-user surface, not in the prelude).

```rust
/// A resolvable source of secret values. Backends implement this; core owns
/// the trait and the one resolution pass that consults it during settings load.
#[async_trait]
pub trait SecretsProvider: Send + Sync {
    /// Provider name for diagnostics / system checks ("env", "aws-sm", "vault").
    fn name(&self) -> &'static str;

    /// Fetch the current value of a secret by its logical key. `Ok(None)` means
    /// "this provider does not hold that key" (so the resolver can fall through
    /// to the next provider); `Err` is a real backend failure (network, auth).
    async fn get(&self, key: &str) -> Result<Option<SecretValue>, SecretsError>;

    /// Metadata without the value: version id, created-at, rotation window,
    /// tags. Used by system checks and `umbral secrets status`. Default: a
    /// bare record derived from `get` when the backend has no metadata API.
    async fn metadata(&self, key: &str) -> Result<Option<SecretMetadata>, SecretsError> {
        Ok(self.get(key).await?.map(|v| v.metadata))
    }

    /// Rotate a secret: generate-or-accept a new version and make it current,
    /// returning the new metadata. Backends that cannot rotate (env) return
    /// `Err(SecretsError::Unsupported)`. See the rotation workflow below.
    async fn rotate(&self, _key: &str, _new: RotationInput) -> Result<SecretMetadata, SecretsError> {
        Err(SecretsError::Unsupported { provider: self.name(), op: "rotate" })
    }
}

pub struct SecretValue {
    pub value: SecretString,       // zeroizing wrapper; never Debug-printed in plaintext
    pub metadata: SecretMetadata,
}

pub struct SecretMetadata {
    pub key: String,
    pub version: Option<String>,          // provider version id / AWS VersionId / Vault version
    pub created_at: Option<DateTime<Utc>>,
    pub rotation_period: Option<Duration>,// declared cadence, for the stale check
    pub last_rotated_at: Option<DateTime<Utc>>,
    pub tags: HashMap<String, String>,
}

pub enum SecretsError {
    NotFound { key: String },
    Backend { provider: &'static str, source: BoxError },
    Auth { provider: &'static str, detail: String },
    Unsupported { provider: &'static str, op: &'static str },
}
```

`SecretString` wraps the plaintext, implements `Debug`/`Display` as `***`, and zeroizes on drop (via the `zeroize` crate). This is the same discipline the redacting `Settings` `Debug` already applies, made a type rather than a per-field newtype.

Config uses a **reference syntax**, not the secret value, in `umbral.toml` / env. A field opts into provider resolution by carrying a `secret://` URI instead of a literal:

```toml
secret_key   = "secret://vault/umbral/prod#secret_key"
database_url = "secret://aws-sm/prod/umbral/db-url"
```

```
UMBRAL_SECRET_KEY=secret://gcp-sm/projects/acme/secrets/umbral-secret-key/versions/latest
```

A value with no `secret://` scheme is a plaintext literal exactly as today (100% backwards compatible; the env backend below also lets you keep everything in env with zero URIs).

### Backends

Each backend is a `SecretsProvider` impl. The env backend lives in core (it is the zero-dependency default and the fallback the resolver always has). Cloud backends are optional crates so an app that does not use them pulls in no AWS/GCP SDK:

| Backend | Crate | `secret://` authority | Notes |
|---|---|---|---|
| Local env / dotenv | `umbral-core` (built in) | `env` | `secret://env/OPENAI_API_KEY` reads `UMBRAL_OPENAI_API_KEY` / `.env`. `rotate` unsupported. This is also the implicit fallback for any plaintext value, so nothing changes for existing apps. |
| AWS Secrets Manager | `umbral-secrets-aws` | `aws-sm` | Uses `aws-sdk-secretsmanager`. `get` = `GetSecretValue`; `metadata` = `DescribeSecret` (version stages, `LastRotatedDate`); `rotate` = `PutSecretValue` + move the `AWSCURRENT` stage, or trigger a configured Lambda rotation. |
| GCP Secret Manager | `umbral-secrets-gcp` | `gcp-sm` | `AccessSecretVersion` for `get`; `GetSecret` for metadata; `AddSecretVersion` + disable-old for `rotate`. |
| Azure Key Vault | `umbral-secrets-azure` | `azure-kv` | `GetSecret`; version list for metadata; `SetSecret` for rotate. |
| HashiCorp Vault | `umbral-secrets-vault` | `vault` | KV v2 engine: `get` reads `data/<path>`; metadata reads `metadata/<path>` (version, `created_time`); `rotate` writes a new version. Also supports Vault's dynamic DB secrets for `database_url` (lease-bound, auto-renewed). |

These crates are NOT under `plugins/` because they are not `Plugin`s; they contribute no routes / models / migrations. They are provider crates the app registers on the builder. They depend only on the `umbral` facade plus their cloud SDK.

Registration mirrors `.plugin(...)`:

```rust
App::builder()
    .secrets_provider(VaultProvider::from_env())   // "vault"
    .secrets_provider(AwsSecretsProvider::new())    // "aws-sm"
    .plugin(SecurityPlugin::new())
    .build().await?;
```

The builder collects providers into a `SecretsResolver` (a small registry keyed by provider name). Order matters only for the `env` fallback; a `secret://<authority>/...` URI dispatches to the named provider directly.

### How Settings resolves secrets through the provider

One new pass in the settings-load path (`fn load` in `settings.rs`), after Figment produces the raw `Settings` and before it is frozen into the `OnceLock`:

1. Figment loads `Settings` as today. Fields now may hold `secret://` URIs (plaintext strings, so no schema change to the struct).
2. `SecretsResolver::resolve_into(&mut settings)` walks the known secret-bearing fields (`secret_key`, `database_url`, each value in `databases`, and each value in `extra`), and for any that parse as a `secret://` URI, calls the matching provider's `get`, replacing the URI with the fetched `SecretString`. A URI whose authority has no registered provider is a hard boot error with the exact provider name that is missing (fail boot, not prod).
3. Resolution runs on the async runtime that is already up during `App::build()`. There is no chicken-and-egg with the DB pool because secrets resolve *before* the pool is opened from `database_url`.
4. `metadata` for each resolved secret is cached alongside settings (a sibling `OnceLock<SecretsSnapshot>`) so system checks and the `umbral secrets status` command can report version / age without re-hitting the backend.

Only `secret_key`, `database_url`, `databases`, and `extra` values are scanned, because those are the only fields whose type is a free string an operator would point at a vault. Named scalar settings (timeouts, host lists) are never secret-resolved.

The ambient-access story is unchanged: user and plugin code still read `umbral::settings::get().secret_key`, and by the time any request is served the value is the resolved plaintext. The provider indirection is invisible past boot.

### Rotation workflow

Rotation has two layers: rotating the value in the backend, and getting the running process to pick up the new value.

1. **Rotate in the backend.** `umbral secrets rotate <key>` (new CLI subcommand) resolves the provider for `<key>`, calls `provider.rotate(key, RotationInput::Generate)` for a framework-generated value (e.g. a fresh 32-byte `secret_key`) or `RotationInput::Explicit(value)` for an operator-supplied one, and prints the new `SecretMetadata` (version, `last_rotated_at`). For AWS this can instead hand off to the managed rotation Lambda.
2. **Overlap / grace window for `secret_key`.** A hard swap of `secret_key` would invalidate every in-flight signed CSRF token and any other HMAC keyed on it. So `secret_key` rotation supports a **key ring**: `secret_key` (current, used to sign) plus an optional `secret_key_previous` (accepted for verification during the overlap). This is a small additive change: signing consumers verify against current-then-previous, sign only with current. `plugins/umbral-security/src/lib.rs` `csrf_valid` gains a second acceptable secret during the window; the CSRF middleware already re-mints on the next safe request, so tokens converge automatically (the exact same convergence mechanism it uses today for the unsigned-to-signed upgrade). Documented as the reason rotation is safe to run on a live fleet.
3. **Pick-up.** Because `Settings` is a frozen `OnceLock`, the running process holds the pre-rotation value until restart. Two supported modes: (a) rolling restart after rotation (the operations-reference default; simplest and race-free), and (b) an optional `SecretsRefresh` background task (opt-in, tasks-plugin-driven) that re-runs `resolve_into` on a schedule for providers that support versioned reads and swaps values behind an `ArcSwap` rather than the `OnceLock`. Mode (b) is a follow-up, gated on demand; mode (a) ships first because it is honest and needs no new global-mutability machinery.

### Boot-time system checks (ties to EnterprisePreset)

The enterprise-preset design (`docs/decisions/2026-08-08-enterprise-preset-design.md`) lists "`secret_key` is not the default or empty" as a production boot check. This feature supplies the check, extending the existing boot-time system-check mechanism (the same one `SecurityPlugin::on_ready` uses), NOT a parallel one:

- **Default-key check (new, closes the real gap).** Under `Environment::Prod`, fail boot if `secret_key == default_secret_key()` (the literal `"umbral-insecure-dev-key-change-me"`). Today's `check_secret_key` only rejects the *empty* key; the default insecure key sails through. The check compares against the exact constant so it cannot drift. Under `Dev` / `Test` it warns loudly instead of failing, matching the existing empty-key behaviour.
- **Empty-key check.** Keep the existing `check_secret_key` behaviour (empty is fatal in prod). It moves next to the default-key check so both live in one place.
- **Unresolved-URI check.** If any secret-bearing field still holds a `secret://` URI after the resolution pass (provider returned `NotFound`, or a plaintext leaked through), fail boot naming the field and URI.
- **Stale-secret check (metadata-driven).** For each resolved secret whose `SecretMetadata` carries a `rotation_period` and `last_rotated_at`, warn (not fail) when `now - last_rotated_at > rotation_period`. This surfaces "your prod signing key has not rotated in 400 days" at boot and in `umbral secrets status`, without making rotation mandatory.

The `EnterprisePreset` bundles these under its production posture so an operator gets them by adding the preset, but they are also available to any app that mounts the check directly (the preset is composition, not privilege).

### Core changes required (#18), minimized

- New `crates/umbral-core/src/secrets.rs`: `SecretsProvider` trait, `SecretValue`, `SecretMetadata`, `SecretsError`, `SecretString`, the `env` provider, and `SecretsResolver`.
- `settings.rs`: one `resolve_into` pass in `load`; the sibling `SecretsSnapshot` `OnceLock`; the default-key / unresolved-URI / stale checks. No change to the `Settings` field types.
- `App::builder().secrets_provider(...)`: collect providers, build the resolver, run it during `build()`.
- Facade: re-export `umbral::secrets::*` (not in the prelude).
- CLI: `umbral secrets status` and `umbral secrets rotate <key>`.

Everything else (the four cloud backends) is out-of-core optional crates. That split is the structural proof that "a secrets backend is a plugin-shaped optional", the same bet as serializers-are-a-plugin.

---

## Part 2: Abuse controls / request firewall (#19)

### What exists today

- **Throttling.** `crates/umbral-core/src/ratelimit.rs` owns the primitive: `Rate` (a budget), `RateLimiter` (per-key sliding-window timestamp store), and `RateDecision` (`RateLimiter::check(key) -> RateDecision`, plus `check_at` for deterministic tests). `plugins/umbral-auth/src/throttle.rs` wraps it as `Throttle` and applies auth-specific policy: login keyed on `ip + "\0" + username` (5 / 5 min), register keyed on `ip` (10 / hour), email actions keyed on `ip + "\0" + email` (5 / hour), returning **429** before DB work. The store is a process-local `HashMap` in a `OnceLock` (`AUTH_THROTTLE`), single-instance (multi-replica effective budget is `max * replicas`, a known limitation).
- **Client IP.** `umbral::settings::client_ip(&HeaderMap)` (settings.rs:155) derives the caller IP from `X-Forwarded-For` honouring `trusted_proxy_hops` (default 0 = trust nothing). This is the one trustworthy IP source and every abuse control keys off it.
- **CSRF + security headers.** `plugins/umbral-security/src/lib.rs`, layered via `Plugin::wrap_router`.

What is missing: a bot score / captcha / challenge framework, IP reputation and allow / deny lists, automated lockout on repeated bad events, honeypots, and a request-firewall DSL that composes all of it. Those are #19.

### Shape: `umbral-abuse`, a plugin, near-zero core change

`umbral-abuse` is a new crate under `plugins/`. It is a normal `Plugin`: it contributes middleware (via `wrap_router`), optionally a model + migration (for persistent deny lists and lockout events), a settings schema, and an `on_ready` check. It reuses `RateLimiter` for counting and `client_ip` for keying, so it introduces no new IP-trust surface and no new counting primitive. The only core touch is exposing a bot-signal extension point (below), which is additive.

### The firewall rule model

The center is a declarative `Firewall` value (a struct, matching the `SecurityConfig` house style of "config is a struct, not a builder chain"), evaluated per request by one middleware layer. It is an ordered list of rules; the first matching rule's action wins.

```rust
pub struct Firewall {
    pub rules: Vec<Rule>,
    /// Default action when no rule matches. Default `Allow`.
    pub default: Action,
}

pub struct Rule {
    pub when: Match,      // condition, ANDed fields
    pub then: Action,     // what to do on match
}

pub enum Match {
    Path(PathPattern),          // prefix / glob on the request path
    Method(Vec<Method>),
    IpIn(IpList),               // named allow / deny list (CIDR-aware)
    CountryIn(Vec<String>),     // via a geo provider adapter (optional)
    BotScoreAbove(u8),          // 0..=100 from the bot-signal chain
    RateExceeded(RateRef),      // a named Rate budget, keyed per `KeyBy`
    HeaderMissing(String),      // e.g. no User-Agent
    Any(Vec<Match>),            // OR
    All(Vec<Match>),            // explicit AND
}

pub enum Action {
    Allow,                      // stop; let it through
    Deny { status: u16 },       // block (default 403), before any handler / DB work
    Challenge(ChallengeKind),   // require a captcha / interstitial before proceeding
    Throttle(RateRef),          // consume a budget; 429 when exhausted
    Lockout(LockoutRef),        // record an event; escalate per the lockout policy
    Tarpit { delay: Duration }, // deliberate slow response for bots
    Log,                        // observe only (shadow / tuning mode)
}
```

`KeyBy` (how a rule keys its counter) reuses the auth-throttle convention: `Ip`, `IpAndPath`, `IpAndHeader(name)`, or a custom closure. IP is always `settings::client_ip`, never a raw header, so a rule cannot be dodged by forging `X-Forwarded-For`.

Every rule action ultimately funnels through the existing primitives: `Throttle` / `Lockout` call `RateLimiter::check`, `Deny` short-circuits with a status the way CSRF's `forbidden()` does, and `Challenge` hands off to a captcha adapter. Nothing re-implements sliding windows.

### Composing the pieces the ticket asks for

**1. Throttles (existing, generalized).** A `Rate` + `RateLimiter` per named budget, exactly as auth uses today. The abuse plugin lets an app declare app-wide budgets (`RateRef::named("api-write", Rate::per_minute(60))`) and key them via `KeyBy`, giving the same 429-before-work behaviour beyond just auth routes. The auth plugin's own throttles stay where they are; `umbral-abuse` does not take them over (single responsibility), it adds route-general ones.

**2. Captcha adapters.** A `CaptchaProvider` trait with adapters for reCAPTCHA (v2 / v3), Cloudflare Turnstile, and hCaptcha:

```rust
#[async_trait]
pub trait CaptchaProvider: Send + Sync {
    fn name(&self) -> &'static str;
    /// Client-side widget config (site key, script URL) for template rendering.
    fn widget(&self) -> CaptchaWidget;
    /// Server-side verify of the token the client submitted, against the caller IP.
    async fn verify(&self, token: &str, remote_ip: Option<&str>) -> Result<CaptchaOutcome, CaptchaError>;
}
```

`CaptchaOutcome` carries pass/fail plus, for score-based providers (reCAPTCHA v3, Turnstile), a normalized 0..=100 score that feeds `BotScoreAbove`. The secret verify keys are read through the #18 secrets path (`settings.extra_str("recaptcha_secret")` today, `secret://...` once #18 lands). `Action::Challenge` renders the provider's widget on an interstitial and gates the original request until `verify` passes; the pass is remembered for a short signed window (HMAC on `secret_key`, same tool as CSRF) so a human is not re-challenged on every click.

**3. IP allow / deny lists.** An `IpList` is a named, CIDR-aware set. Two sources compose: a static list from config (`umbral.toml`) and a dynamic list persisted in a small model (`AbuseIpEntry { ip_or_cidr, kind: Allow|Deny, reason, expires_at }`) owned by the plugin's migration. Allow always beats deny (an operator's own monitoring IP is never locked out). This is where "IP reputation" plugs in later: a reputation feed is just another `IpList` source adapter, no new rule surface.

**4. Honeypots.** Two forms, both plugin-provided:
   - **Form honeypot.** A hidden field (`display:none`) the framework injects into forms; a bot that fills it triggers `Action::Lockout` on that IP. Renders via the same template hook CSRF uses for `{{ csrf_input }}`, so it is one `{{ honeypot_field }}` in a template.
   - **Path honeypot.** Configured trap paths (`/wp-login.php`, `/.env`) that no real client hits; a request there is immediate evidence of scanning and feeds the lockout / deny list. This is a `Rule` with `Match::Path` + `Action::Lockout`, so it needs no special machinery.

**5. Event-based lockout.** A `Lockout` policy is a `RateLimiter` over *bad events* rather than requests: N failed logins / honeypot hits / 403s from one IP within a window escalates to a temporary deny (writes an `AbuseIpEntry` with `expires_at`), so the IP is blocked at the firewall layer for the cooldown without per-route code. This is the generalization of what auth's login throttle does per-account, lifted to an IP-wide, event-typed policy. Escalation is tiered (warn -> challenge -> temp-deny -> longer temp-deny) and every tier is data in the policy, not code.

### Middleware placement

One `wrap_router` layer, ordered *outside* CSRF and auth so a denied / throttled request is rejected before any heavier middleware or DB work runs (the same reasoning that puts the auth throttle at handler entry, and that layers `RequestBodyLimitLayer` outermost in `umbral-security`). The layer:

1. Derives the caller IP via `settings::client_ip` once.
2. Evaluates the `Firewall` rules in order; first match wins.
3. Executes the action: short-circuit for `Deny` / exhausted `Throttle` / `Lockout`-blocked, render the interstitial for `Challenge`, delay for `Tarpit`, otherwise call the handler.

A **shadow mode** (`Action::Log` as the effective action for a rule, or a global `Firewall::observe_only`) logs what *would* have happened without blocking, so an operator can tune rules against real traffic before enforcing. This is the honest way to ship an abuse layer that could otherwise lock out real users.

### Multi-replica honesty

Like the auth throttle, the in-memory `RateLimiter` counts per replica, so budgets and lockouts are per-replica until a shared backend exists. This design keys everything off the same `RateLimiter` seam, so when the distributed throttle backend (gaps5 #67) lands, `umbral-abuse` inherits a global limiter by swapping the store, with no rule-surface change. Until then the plugin documents the `max * replicas` caveat exactly as auth does, and the `EnterprisePreset` warns when multi-replica is configured without a shared backend.

### Core changes required (#19), minimized

- **Bot-signal extension point (additive).** A small trait `BotSignal` (input: request head + IP; output: a 0..=100 partial score / veto) so captcha outcomes, reputation feeds, and heuristics (missing User-Agent, known-bad ASN) compose into one `BotScoreAbove` input. This is the one new core seam; it is a trait object registry like `Plugin`, no concrete signal named in core.
- Everything else is in `plugins/umbral-abuse/`: the `Firewall` model, `Rule` / `Match` / `Action`, `CaptchaProvider` + the three adapters, `IpList` + `AbuseIpEntry` model + migration, honeypot template hook, lockout policy, and the `wrap_router` layer.
- Reuses without change: `umbral::ratelimit::{Rate, RateLimiter, RateDecision}`, `umbral::settings::client_ip`, the `secret_key` HMAC helpers, and the template CSRF-style injection hook.

`umbral-abuse` depends only on the `umbral` facade, like every other plugin. Its captcha secret keys resolve through the #18 secrets path, which is the one place the two features touch.

---

## Summary of the split (both features)

| Concern | Core (umbral-core) | Optional crate / plugin |
|---|---|---|
| #18 provider trait, resolver, env backend, settings resolution pass, boot checks | yes (narrow seam, unavoidable due to boot ordering) | AWS / GCP / Azure / Vault backends |
| #18 rotation CLI + key-ring overlap for `secret_key` | CLI in `umbral-cli`; key-ring verify in `umbral-security` | managed-rotation hooks per cloud backend |
| #19 firewall, captcha, IP lists, honeypots, lockout | only the additive `BotSignal` seam | all of it in `plugins/umbral-abuse` |

Both honor the motto: the smallest possible thing lives in core (a trait plus one resolution pass for #18; a single extension trait for #19), and every concrete capability is a plugin or optional crate that depends inward on the facade. The one place core genuinely must own logic is secrets resolution, because it runs before plugins do; this draft states that plainly rather than forcing a pure-plugin design that could not work.

## Follow-ups and ties

- Feeds the `EnterprisePreset` production boot checks (gaps5 #3): default-key, stale-key, unresolved-URI (#18); multi-replica-without-shared-backend warning (#19).
- #19's global limiter is gated on distributed throttling (gaps5 #67); ships single-replica first with the documented caveat.
- #18 mode (b) live secret refresh is a follow-up gated on demand; mode (a) rolling restart ships first.
- Each feature ships its user-facing doc page under `documentation/docs/v0.0.1/` (`plugins/secrets.mdx`, `plugins/abuse.mdx`) when implemented, per the ship-a-feature-ship-its-doc rule.
