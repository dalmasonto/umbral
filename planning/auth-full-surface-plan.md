# umbral-auth Full Auth Surface — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Grow `umbral-auth` into a complete auth system — email verification (6-digit codes), password forgot/reset (tokenized links), a single reusable `logout`, a pluggable mailer — exposed both as JSON (under the REST/OpenAPI base path) and as server-rendered Jinja pages, sharing one core.

**Architecture:** A shared core of async functions in `umbral-auth` does the work (ORM-only, no raw SQL). Two thin surfaces call it: JSON handlers (extend `auth_routes.rs`) and HTML/Jinja handlers (new `template_routes.rs`). Email is decoupled via an `AuthMailer` trait (console default; app wires a closure). Secrets (codes/tokens) live in a new hashed-at-rest `AuthChallenge` table. The JSON base path auto-follows the REST plugin via a new `umbral::web::api_base()` core ambient.

**Tech Stack:** Rust, axum 0.8, sqlx (SQLite/PG via the ORM), MiniJinja (`umbral::templates`), argon2, chrono, async-trait. Design note: `docs/decisions/2026-06-28-auth-full-surface.md`.

## Global Constraints

- Plugin code uses the ORM only — never `sqlx::query`/`query_as` for row I/O (CLAUDE.md). The new flows use `Model::objects()`.
- `umbral-auth` must NOT take a Cargo dependency on `umbral-rest` or `umbral-email` (REST-free / mail-free apps must compile without them).
- All new deps are already in `plugins/umbral-auth/Cargo.toml` (chrono, serde, serde_json, sqlx, rand, base64, sha2, async-trait, axum, http, tokio, tracing, umbral-sessions). No Cargo changes in umbral-auth.
- Secure by default: enumeration responses are generic; codes 6-digit / 15-min TTL / 5-attempt cap / single-use; reset tokens 256-bit / 1-hour TTL / single-use; password-strength policy applies to resets.
- Never wipe the DB / delete migrations to apply schema changes (CLAUDE.md).
- Per commit: `cargo fmt && cargo clippy --all-targets && cargo build && cargo test` for the touched crates before each task's commit.
- Spec field-const modules: a model `Foo` (table `foo`) generates `foo::FIELD` consts; filters read e.g. `auth_challenge::USER_ID`.

---

## File structure

- `crates/umbral-core/src/web.rs` — add `api_base()` + `set_api_base()` ambient (re-exported via `umbral::web::*`).
- `plugins/umbral-rest/src/lib.rs` — publish base path into the ambient in an early build phase.
- `plugins/umbral-sessions/src/lib.rs` — add `revoke_user_sessions(user_id_str)`.
- `plugins/umbral-auth/src/lib.rs` — `email_verified_at` column; register `AuthChallenge`; builder methods (`mailer`, `require_verified_email`, `email_verification`, `with_template_pages[_at]`); `on_ready` seals the mailer; wire both route surfaces; JSON prefix resolution.
- `plugins/umbral-auth/src/mailer.rs` (new) — `AuthMailer` trait, `OutgoingMail`, `AuthMailError`, `ConsoleMailer`, ambient mailer.
- `plugins/umbral-auth/src/challenge.rs` (new) — `AuthChallenge` model + code/token generation + create/lookup/consume + the four core flow fns.
- `plugins/umbral-auth/src/auth_routes.rs` — JSON handlers for verify-email/resend/forgot/reset; expose `logout`; openapi entries; prefix from plugin.
- `plugins/umbral-auth/src/template_routes.rs` (new) — Jinja page handlers.
- `plugins/umbral-auth/templates/auth/*.html` (new) — base + pages + email bodies.
- `plugins/umbral-auth/tests/*` (new files per flow).
- `documentation/docs/v0.0.1/auth/*.mdx` (new pages).

---

### Task 1: Core `api_base` ambient

**Files:**
- Modify: `crates/umbral-core/src/web.rs` (append near the bottom, module scope)
- Test: `crates/umbral-core/src/web.rs` (`#[cfg(test)]` mod, or the crate's existing web tests file)

**Interfaces:**
- Produces: `umbral_core::web::api_base() -> String` (default `"/api"`); `umbral_core::web::set_api_base(base: impl Into<String>)` (first-call-wins). Surfaced as `umbral::web::api_base` / `umbral::web::set_api_base` via the existing `pub use umbral_core::web::*`.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod api_base_tests {
    use super::*;
    #[test]
    fn api_base_defaults_to_api_then_takes_first_set() {
        // Default before any set.
        assert_eq!(api_base(), "/api");
        set_api_base("/v2");
        assert_eq!(api_base(), "/v2");
        // First-set-wins: a later set is ignored.
        set_api_base("/v3");
        assert_eq!(api_base(), "/v2");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbral-core api_base_defaults_to_api -- --nocapture`
Expected: FAIL — `cannot find function api_base`.

- [ ] **Step 3: Implement**

```rust
use std::sync::OnceLock;

/// Process-wide API base path, published by the REST plugin during build
/// (before router assembly) so other plugins — notably umbral-auth — can
/// mount their JSON routes under the same prefix without a Cargo dependency
/// on umbral-rest. Defaults to "/api" when no REST plugin set it.
static API_BASE: OnceLock<String> = OnceLock::new();

/// Read the configured API base path. Returns "/api" until [`set_api_base`]
/// runs. Trailing slashes are not normalized here — callers append "/auth"
/// etc. directly, and the REST plugin publishes its own normalized base.
pub fn api_base() -> String {
    API_BASE.get().cloned().unwrap_or_else(|| "/api".to_string())
}

/// Publish the API base path. First call wins (mirrors the REST plugin's
/// own `CONFIG` OnceLock); subsequent calls are ignored. The REST plugin
/// calls this in an early build phase.
pub fn set_api_base(base: impl Into<String>) {
    let _ = API_BASE.set(base.into());
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p umbral-core api_base_defaults_to_api`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/umbral-core/src/web.rs
git commit -m "feat(core): add umbral::web::api_base ambient for cross-plugin base-path discovery"
```

---

### Task 2: REST publishes its base path early

**Files:**
- Modify: `plugins/umbral-rest/src/lib.rs` — in `impl Plugin for RestPlugin`, the `models()` method (publish as its first line; if `models()` isn't implemented, add it returning the existing/empty model list).
- Test: `plugins/umbral-rest/tests/api_base.rs` (new — its own test binary so the OnceLock isn't shared with other rest tests)

**Interfaces:**
- Consumes: `umbral::web::set_api_base` (Task 1), `RestPlugin::base_path()`.
- Produces: after `App::build()` with a `RestPlugin`, `umbral::web::api_base()` equals the REST base path.

- [ ] **Step 1: Write the failing test**

```rust
#![allow(dead_code, private_interfaces)]
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use umbral_rest::RestPlugin;

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Thing { id: i64, name: String }

#[tokio::test]
async fn rest_publishes_custom_base_into_api_base() {
    let settings = umbral::Settings::from_env().expect("settings");
    let tmp = tempfile::tempdir().expect("tmp");
    std::mem::forget(tmp);
    let pool = SqlitePoolOptions::new().max_connections(1)
        .connect_with(SqliteConnectOptions::new().filename(":memory:").create_if_missing(true))
        .await.expect("pool");

    let _app = umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .model::<Thing>()
        .plugin(RestPlugin::default().at("/v2"))
        .build()
        .expect("build");

    assert_eq!(umbral::web::api_base(), "/v2", "REST should publish its base path");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbral-rest --test api_base`
Expected: FAIL — `api_base()` returns `/api`, not `/v2`.

- [ ] **Step 3: Implement**

In `plugins/umbral-rest/src/lib.rs`, inside `impl<...> Plugin for RestPlugin`, ensure `models()` publishes the base. If `models()` already exists, prepend the call; otherwise add:

```rust
fn models(&self) -> Vec<umbral::migrate::ModelMeta> {
    // Publish our base path before any plugin's routes() runs (models() is
    // collected in an earlier build phase than router assembly), so
    // umbral-auth can mount its JSON routes under the same prefix without a
    // Cargo dependency on this crate. See umbral::web::api_base.
    umbral::web::set_api_base(self.base_path());
    Vec::new() // RestPlugin owns no models; it serves app-registered ones.
}
```

If `models()` was already present and returned a non-empty vec, keep that body and just insert the `set_api_base` line first.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p umbral-rest --test api_base`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add plugins/umbral-rest/src/lib.rs plugins/umbral-rest/tests/api_base.rs
git commit -m "feat(rest): publish base path into umbral::web::api_base at build"
```

---

### Task 3: `revoke_user_sessions` in umbral-sessions

**Files:**
- Modify: `plugins/umbral-sessions/src/lib.rs` (add near `destroy_session`)
- Test: `plugins/umbral-sessions/tests/revoke_user_sessions.rs` (new) — follow an existing sessions test's boot pattern (ambient pool + create `session` table).

**Interfaces:**
- Produces: `pub async fn revoke_user_sessions(user_id_str: &str) -> Result<u64, SessionError>` — deletes every `session` row whose `user_id == Some(user_id_str)`, returns the count. User-agnostic (string PK), so it works for any user model.

- [ ] **Step 1: Write the failing test**

```rust
// boot(): create the `session` table on an ambient SQLite pool exactly like
// the existing sessions tests do (copy their helper). Then:
#[tokio::test]
async fn revoke_removes_all_of_one_users_sessions_only() {
    boot().await;
    // Two sessions for user "7", one for user "9", one anonymous.
    umbral_sessions::create_session(Some("7".into()), std::time::Duration::from_secs(3600)).await.unwrap();
    umbral_sessions::create_session(Some("7".into()), std::time::Duration::from_secs(3600)).await.unwrap();
    umbral_sessions::create_session(Some("9".into()), std::time::Duration::from_secs(3600)).await.unwrap();
    umbral_sessions::create_session(None, std::time::Duration::from_secs(3600)).await.unwrap();

    let removed = umbral_sessions::revoke_user_sessions("7").await.unwrap();
    assert_eq!(removed, 2, "both of user 7's sessions removed");

    // user 9 + anonymous remain.
    let remaining = umbral_sessions::Session::objects().count().await.unwrap();
    assert_eq!(remaining, 2);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbral-sessions --test revoke_user_sessions`
Expected: FAIL — `cannot find function revoke_user_sessions`.

- [ ] **Step 3: Implement**

```rust
/// Delete every session row owned by `user_id_str` — the "log out
/// everywhere" primitive. Used after a password reset/change so stolen
/// session cookies stop working. Anonymous sessions (`user_id IS NULL`) are
/// never matched. Returns the number of rows removed.
pub async fn revoke_user_sessions(user_id_str: &str) -> Result<u64, SessionError> {
    let removed = Session::objects()
        .filter(session::USER_ID.eq(Some(user_id_str.to_string())))
        .delete()
        .await?;
    Ok(removed)
}
```

If `QuerySet::delete()` returns `()` rather than a count in this codebase, change the signature to `Result<(), SessionError>` and drop the count assertion in the test (assert remaining == 2 only). Verify against `crates/umbral-core/src/orm` `delete` signature before writing the test.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p umbral-sessions --test revoke_user_sessions`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add plugins/umbral-sessions/src/lib.rs plugins/umbral-sessions/tests/revoke_user_sessions.rs
git commit -m "feat(sessions): add revoke_user_sessions (log-out-everywhere primitive)"
```

---

### Task 4: `email_verified_at` column + `AuthChallenge` model

**Files:**
- Modify: `plugins/umbral-auth/src/lib.rs` — add the field to `AuthUser`; register `AuthChallenge` in `models()`.
- Create: `plugins/umbral-auth/src/challenge.rs` (model only in this task; flow fns land in Task 6/8/9).
- Test: `plugins/umbral-auth/tests/challenge_model.rs` (new)

**Interfaces:**
- Produces: `AuthUser.email_verified_at: Option<DateTime<Utc>>`; `umbral_auth::AuthChallenge` model with fields `id, user_id, purpose, secret_hash, expires_at, attempts, used_at, created_at`; field consts module `auth_challenge::*`. `models()` returns `[U, AuthToken, AuthChallenge]` when `U = AuthUser`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn auth_user_has_email_verified_at_and_challenge_table_registered() {
    use umbral::migrate::ModelMeta;
    let user = ModelMeta::for_::<umbral_auth::AuthUser>();
    assert!(user.fields.iter().any(|c| c.name == "email_verified_at" && c.nullable),
        "AuthUser must expose a nullable email_verified_at column");
    let ch = ModelMeta::for_::<umbral_auth::AuthChallenge>();
    assert_eq!(ch.table, "auth_challenge");
    for f in ["user_id", "purpose", "secret_hash", "expires_at", "attempts", "used_at", "created_at"] {
        assert!(ch.fields.iter().any(|c| c.name == f), "AuthChallenge missing column {f}");
    }
}
```

(Confirm `ModelMeta`'s field accessor names — `fields`, `.name`, `.nullable`, `.table` — against `crates/umbral-core/src/migrate`; adjust if the struct differs.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbral-auth --test challenge_model`
Expected: FAIL — `AuthChallenge` not found / `email_verified_at` missing.

- [ ] **Step 3: Implement**

In `lib.rs`, add to `AuthUser` after `last_login`:

```rust
    /// When this user's email was verified, NULL until they complete the
    /// verification flow. Tracked always; only enforced when the plugin is
    /// built with `require_verified_email()`.
    pub email_verified_at: Option<DateTime<Utc>>,
```

Create `challenge.rs`:

```rust
//! Short-lived, single-use, hashed-at-rest secrets for the email-verification
//! and password-reset flows. One table, discriminated by `purpose`.

use crate::AuthUser;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use umbral::orm::ForeignKey;

/// Stored discriminator values for [`AuthChallenge::purpose`].
pub const PURPOSE_EMAIL_VERIFY: &str = "email_verify";
pub const PURPOSE_PASSWORD_RESET: &str = "password_reset";

/// One pending challenge. The plaintext (6-digit code or opaque token) is
/// never stored — only `base64(sha256(plaintext))`. Single-use (`used_at`),
/// time-boxed (`expires_at`), and (for codes) attempt-capped (`attempts`).
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
pub struct AuthChallenge {
    pub id: i64,
    #[umbral(on_delete = "cascade")]
    pub user_id: ForeignKey<AuthUser>,
    #[umbral(max_length = 32)]
    pub purpose: String,
    #[umbral(max_length = 64)]
    pub secret_hash: String,
    pub expires_at: DateTime<Utc>,
    pub attempts: i32,
    pub used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}
```

In `lib.rs` add `pub mod challenge;` and `pub use challenge::AuthChallenge;`, and extend `models()`:

```rust
fn models(&self) -> Vec<umbral::migrate::ModelMeta> {
    let mut models = vec![umbral::migrate::ModelMeta::for_::<U>()];
    if std::any::TypeId::of::<U>() == std::any::TypeId::of::<AuthUser>() {
        models.push(umbral::migrate::ModelMeta::for_::<AuthToken>());
        models.push(umbral::migrate::ModelMeta::for_::<AuthChallenge>());
    }
    models
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p umbral-auth --test challenge_model`
Expected: PASS. Then `cargo build -p umbral-auth` to confirm the `AuthUser` literal is updated everywhere it's constructed (grep `AuthUser {` and add `email_verified_at: None` to each constructor — notably `create_user` and any test fixtures; fix compile errors).

- [ ] **Step 5: Commit**

```bash
git add plugins/umbral-auth/src/lib.rs plugins/umbral-auth/src/challenge.rs plugins/umbral-auth/tests/challenge_model.rs
git commit -m "feat(auth): add email_verified_at + AuthChallenge model"
```

---

### Task 5: Pluggable mailer (`AuthMailer`, `ConsoleMailer`, ambient)

**Files:**
- Create: `plugins/umbral-auth/src/mailer.rs`
- Modify: `plugins/umbral-auth/src/lib.rs` — `pub mod mailer;` + re-exports; add the `mailer` plugin field, `mailer(...)` builder, and seal it in `on_ready`.
- Test: `plugins/umbral-auth/tests/mailer.rs` (new)

**Interfaces:**
- Produces:
  - `pub struct OutgoingMail { pub to: String, pub subject: String, pub html: String, pub text: String }`
  - `pub enum AuthMailError { Send(String) }` (Display + std::error::Error)
  - `#[async_trait] pub trait AuthMailer: Send + Sync { async fn send(&self, mail: OutgoingMail) -> Result<(), AuthMailError>; }` with a blanket impl for `Fn(OutgoingMail) -> impl Future<Output=Result<(),AuthMailError>>`.
  - `pub struct ConsoleMailer;` (default).
  - `pub(crate) fn active_mailer() -> std::sync::Arc<dyn AuthMailer>` (returns `ConsoleMailer` if unset).
  - `AuthPlugin::mailer(impl AuthMailer + 'static) -> Self`.

- [ ] **Step 1: Write the failing test**

```rust
use std::sync::{Arc, Mutex};
use umbral_auth::mailer::{AuthMailer, OutgoingMail};

#[derive(Default, Clone)]
struct Recorder(Arc<Mutex<Vec<OutgoingMail>>>);
#[async_trait::async_trait]
impl AuthMailer for Recorder {
    async fn send(&self, mail: OutgoingMail) -> Result<(), umbral_auth::mailer::AuthMailError> {
        self.0.lock().unwrap().push(mail);
        Ok(())
    }
}

#[tokio::test]
async fn recorder_mailer_captures_and_closure_impl_works() {
    let rec = Recorder::default();
    rec.send(OutgoingMail { to: "a@b.c".into(), subject: "s".into(), html: "<b>h</b>".into(), text: "t".into() })
        .await.unwrap();
    assert_eq!(rec.0.lock().unwrap().len(), 1);
    assert_eq!(rec.0.lock().unwrap()[0].to, "a@b.c");

    // Closure blanket impl: a plain async closure is an AuthMailer.
    let hits = Arc::new(Mutex::new(0));
    let h2 = hits.clone();
    let closure = move |_m: OutgoingMail| {
        let h = h2.clone();
        async move { *h.lock().unwrap() += 1; Ok(()) }
    };
    AuthMailer::send(&closure, OutgoingMail { to: "x".into(), subject: "".into(), html: "".into(), text: "".into() })
        .await.unwrap();
    assert_eq!(*hits.lock().unwrap(), 1);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbral-auth --test mailer`
Expected: FAIL — `umbral_auth::mailer` not found.

- [ ] **Step 3: Implement** — `mailer.rs`:

```rust
//! The pluggable email seam. umbral-auth renders bodies via
//! `umbral::templates` and hands them to whatever `AuthMailer` the app wired
//! (default: print to stderr). Keeps auth decoupled from any mail crate.

use async_trait::async_trait;
use std::future::Future;
use std::sync::{Arc, OnceLock};

/// A rendered message ready to transmit.
#[derive(Debug, Clone)]
pub struct OutgoingMail {
    pub to: String,
    pub subject: String,
    pub html: String,
    pub text: String,
}

/// Failure to hand a message to the transport.
#[derive(Debug)]
pub enum AuthMailError {
    Send(String),
}
impl std::fmt::Display for AuthMailError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthMailError::Send(m) => write!(f, "failed to send auth email: {m}"),
        }
    }
}
impl std::error::Error for AuthMailError {}

/// What the app wires in. Implement for a type, or pass an async closure
/// `Fn(OutgoingMail) -> Future<Output = Result<(), AuthMailError>>` (blanket
/// impl below). Delegate to `umbral_email::send` in one line if you use it.
#[async_trait]
pub trait AuthMailer: Send + Sync {
    async fn send(&self, mail: OutgoingMail) -> Result<(), AuthMailError>;
}

#[async_trait]
impl<F, Fut> AuthMailer for F
where
    F: Fn(OutgoingMail) -> Fut + Send + Sync,
    Fut: Future<Output = Result<(), AuthMailError>> + Send,
{
    async fn send(&self, mail: OutgoingMail) -> Result<(), AuthMailError> {
        self(mail).await
    }
}

/// Default mailer: print the message to stderr (dev-visible code/link) and
/// log a loud warning if it's the active mailer outside Dev/Test.
pub struct ConsoleMailer;

#[async_trait]
impl AuthMailer for ConsoleMailer {
    async fn send(&self, mail: OutgoingMail) -> Result<(), AuthMailError> {
        let prod = umbral::settings::get_opt()
            .map(|s| !matches!(s.environment, umbral::Environment::Dev | umbral::Environment::Test))
            .unwrap_or(false);
        if prod {
            tracing::warn!(
                to = %mail.to,
                "umbral-auth ConsoleMailer is active in a non-Dev environment — auth emails are \
                 only printed, not delivered. Wire AuthPlugin::mailer(...) for production."
            );
        }
        eprintln!(
            "\n--- umbral-auth email ---\nTo: {}\nSubject: {}\n\n{}\n-------------------------\n",
            mail.to, mail.subject, mail.text
        );
        Ok(())
    }
}

static MAILER: OnceLock<Arc<dyn AuthMailer>> = OnceLock::new();

/// The mailer the flow functions use. Falls back to [`ConsoleMailer`].
pub(crate) fn active_mailer() -> Arc<dyn AuthMailer> {
    MAILER.get().cloned().unwrap_or_else(|| Arc::new(ConsoleMailer))
}

/// Install the process mailer. First call wins (mirrors the password policy
/// seal); `on_ready` calls this once at boot.
pub(crate) fn install_mailer(m: Arc<dyn AuthMailer>) {
    let _ = MAILER.set(m);
}
```

In `lib.rs`: add `pub mod mailer;` and `pub use mailer::{AuthMailer, AuthMailError, ConsoleMailer, OutgoingMail};`.

Add a Debug-friendly slot so `#[derive(Debug)]` on `AuthPlugin` still compiles:

```rust
struct MailerSlot(std::sync::Mutex<Option<std::sync::Arc<dyn mailer::AuthMailer>>>);
impl std::fmt::Debug for MailerSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MailerSlot(..)")
    }
}
```

Add field `mailer: MailerSlot,` to `AuthPlugin`, default `MailerSlot(std::sync::Mutex::new(None))`, and the builder (place in the `impl<U: UserModel> AuthPlugin<U>` block):

```rust
/// Wire the mailer used by the verification + password-reset flows.
/// Pass a type implementing [`AuthMailer`] or an async closure
/// `|mail| async { ... }`. Unset → [`ConsoleMailer`] (stderr in dev).
///
/// ```ignore
/// AuthPlugin::<AuthUser>::default().mailer(|m: OutgoingMail| async move {
///     umbral_email::send(&umbral_email::EmailMessage::new(m.subject, vec![m.to])
///         .html_body(m.html).text_body(m.text)).await
///         .map(|_| ()).map_err(|e| AuthMailError::Send(e.to_string()))
/// })
/// ```
pub fn mailer(self, m: impl mailer::AuthMailer + 'static) -> Self {
    *self.mailer.0.lock().expect("mailer slot poisoned") = Some(std::sync::Arc::new(m));
    self
}
```

In `on_ready`, after the existing password-policy seal, add:

```rust
if let Ok(mut guard) = self.mailer.0.lock() {
    if let Some(m) = guard.take() {
        crate::mailer::install_mailer(m);
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p umbral-auth --test mailer`
Expected: PASS. Also `cargo build -p umbral-auth`.

- [ ] **Step 5: Commit**

```bash
git add plugins/umbral-auth/src/mailer.rs plugins/umbral-auth/src/lib.rs plugins/umbral-auth/tests/mailer.rs
git commit -m "feat(auth): pluggable AuthMailer seam with ConsoleMailer default"
```

---

### Task 6: Challenge generation, hashing, create/lookup/consume

**Files:**
- Modify: `plugins/umbral-auth/src/challenge.rs`
- Test: `plugins/umbral-auth/tests/challenge_lifecycle.rs` (new — boot an ambient SQLite pool + create `auth_user`/`auth_challenge` tables; mirror `tests/integration.rs`'s boot)

**Interfaces:**
- Produces (in `challenge` module):
  - `pub(crate) fn generate_code() -> String` (6 digits, zero-padded).
  - `pub(crate) fn generate_reset_token() -> String` (`umbral_` + 43 base64 chars).
  - `pub(crate) fn hash_secret(plaintext: &str) -> String` (reuses `crate::token::digest_token`).
  - `impl AuthChallenge`: `async fn issue(user_id: i64, purpose: &str, plaintext: &str, ttl: Duration) -> Result<AuthChallenge, AuthError>`; `async fn find_active_for_user(user_id: i64, purpose: &str) -> Result<Option<AuthChallenge>, AuthError>`; `async fn find_active_by_secret(plaintext: &str, purpose: &str) -> Result<Option<AuthChallenge>, AuthError>`; `async fn mark_used(&self) -> Result<(), AuthError>`; `async fn bump_attempts(&self) -> Result<(), AuthError>`; `fn is_live(&self) -> bool` (unused + unexpired).
- Consumes: `crate::AuthError`, `crate::token::digest_token`.

- [ ] **Step 1: Write the failing test**

```rust
use std::time::Duration;
use umbral_auth::challenge::{AuthChallenge, PURPOSE_EMAIL_VERIFY};

#[tokio::test]
async fn issue_then_find_then_consume_code() {
    boot().await;
    let user_id = seed_user("alice", "alice@example.com").await; // helper inserts an AuthUser, returns id
    // Issue a code-style challenge with a known plaintext.
    let _c = AuthChallenge::issue(user_id, PURPOSE_EMAIL_VERIFY, "483920", Duration::from_secs(900))
        .await.unwrap();

    // Found by (user, purpose) and live.
    let found = AuthChallenge::find_active_for_user(user_id, PURPOSE_EMAIL_VERIFY).await.unwrap().unwrap();
    assert!(found.is_live());
    assert_eq!(found.attempts, 0);

    // Wrong guess bumps attempts.
    found.bump_attempts().await.unwrap();
    let again = AuthChallenge::find_active_for_user(user_id, PURPOSE_EMAIL_VERIFY).await.unwrap().unwrap();
    assert_eq!(again.attempts, 1);

    // Consume: marked used → no longer live / not returned as active.
    again.mark_used().await.unwrap();
    assert!(AuthChallenge::find_active_for_user(user_id, PURPOSE_EMAIL_VERIFY).await.unwrap().is_none());
}

#[test]
fn generated_code_is_six_digits_and_token_has_prefix() {
    let code = umbral_auth::challenge::generate_code();
    assert_eq!(code.len(), 6);
    assert!(code.chars().all(|c| c.is_ascii_digit()), "code is all digits: {code}");
    let tok = umbral_auth::challenge::generate_reset_token();
    assert!(tok.starts_with("umbral_"));
}
```

(If `generate_code`/`generate_reset_token` should stay `pub(crate)`, expose them through a `#[doc(hidden)] pub fn` test shim or make them `pub` in the `challenge` module for the unit assertion. Prefer a small `#[cfg(test)]`-gated `pub` re-export; or move the digit/prefix asserts into a `#[cfg(test)] mod tests` inside `challenge.rs`.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbral-auth --test challenge_lifecycle`
Expected: FAIL — methods not found.

- [ ] **Step 3: Implement** — append to `challenge.rs`:

```rust
use crate::AuthError;
use crate::token::digest_token;
use rand::Rng;
use std::time::Duration;

pub(crate) fn generate_code() -> String {
    let n: u32 = rand::rngs::OsRng.gen_range(0..1_000_000);
    format!("{n:06}")
}

pub(crate) fn generate_reset_token() -> String {
    // Reuse the bearer-token generator shape: umbral_ + 43 url-safe chars.
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use rand::RngCore;
    let mut buf = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    format!("umbral_{}", URL_SAFE_NO_PAD.encode(buf))
}

pub(crate) fn hash_secret(plaintext: &str) -> String {
    digest_token(plaintext)
}

impl AuthChallenge {
    pub(crate) async fn issue(
        user_id: i64,
        purpose: &str,
        plaintext: &str,
        ttl: Duration,
    ) -> Result<AuthChallenge, AuthError> {
        let now = Utc::now();
        let expires_at = now + chrono::Duration::from_std(ttl).unwrap_or_else(|_| chrono::Duration::minutes(15));
        let row = AuthChallenge::objects()
            .create(AuthChallenge {
                id: 0,
                user_id: ForeignKey::new(user_id),
                purpose: purpose.to_string(),
                secret_hash: hash_secret(plaintext),
                expires_at,
                attempts: 0,
                used_at: None,
                created_at: now,
            })
            .await?;
        Ok(row)
    }

    pub(crate) fn is_live(&self) -> bool {
        self.used_at.is_none() && self.expires_at > Utc::now()
    }

    pub(crate) async fn find_active_for_user(
        user_id: i64,
        purpose: &str,
    ) -> Result<Option<AuthChallenge>, AuthError> {
        let row = AuthChallenge::objects()
            .filter(
                auth_challenge::USER_ID.eq(user_id)
                    & auth_challenge::PURPOSE.eq(purpose.to_string())
                    & auth_challenge::USED_AT.is_null(),
            )
            .order_by(&[auth_challenge::CREATED_AT.desc()])
            .first()
            .await?;
        Ok(row.filter(|c| c.is_live()))
    }

    pub(crate) async fn find_active_by_secret(
        plaintext: &str,
        purpose: &str,
    ) -> Result<Option<AuthChallenge>, AuthError> {
        let row = AuthChallenge::objects()
            .filter(
                auth_challenge::SECRET_HASH.eq(hash_secret(plaintext))
                    & auth_challenge::PURPOSE.eq(purpose.to_string())
                    & auth_challenge::USED_AT.is_null(),
            )
            .first()
            .await?;
        Ok(row.filter(|c| c.is_live()))
    }

    pub(crate) async fn mark_used(&self) -> Result<(), AuthError> {
        let mut delta = serde_json::Map::new();
        delta.insert("used_at".to_string(), serde_json::json!(Utc::now()));
        AuthChallenge::objects()
            .filter(auth_challenge::ID.eq(self.id))
            .update_values(delta)
            .await?;
        Ok(())
    }

    pub(crate) async fn bump_attempts(&self) -> Result<(), AuthError> {
        let mut delta = serde_json::Map::new();
        delta.insert("attempts".to_string(), serde_json::json!(self.attempts + 1));
        AuthChallenge::objects()
            .filter(auth_challenge::ID.eq(self.id))
            .update_values(delta)
            .await?;
        Ok(())
    }
}
```

(Verify the QuerySet API names against `crates/umbral-core/src/orm`: `order_by`, `.desc()`, `is_null()`, `update_values`. The token module already uses `update_values` with a `serde_json::Map` — mirror it exactly. If `is_null()` isn't available, filter `used_at` in Rust after fetch instead.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p umbral-auth --test challenge_lifecycle`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add plugins/umbral-auth/src/challenge.rs plugins/umbral-auth/tests/challenge_lifecycle.rs
git commit -m "feat(auth): challenge generation, hashing, and lifecycle helpers"
```

---

### Task 7: Public `logout`

**Files:**
- Modify: `plugins/umbral-auth/src/lib.rs` (add the fn; re-export); `plugins/umbral-auth/src/auth_routes.rs` (the JSON `logout` handler calls it).
- Test: `plugins/umbral-auth/tests/logout_fn.rs` (new)

**Interfaces:**
- Produces: `pub async fn umbral_auth::logout(req: &http::HeaderMap, resp: &mut http::HeaderMap) -> Result<(), AuthError>`.
- Consumes: `umbral_sessions::logout`.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn logout_clears_the_session_cookie() {
    boot().await; // creates session table on ambient pool
    // Establish a session, capture the cookie.
    let mut set = http::HeaderMap::new();
    umbral_sessions::login_user_id(&http::HeaderMap::new(), &mut set, Some("1".into())).await.unwrap();
    let cookie = set.get(http::header::SET_COOKIE).unwrap().to_str().unwrap().to_string();
    // Build a request carrying that cookie, then log out.
    let mut req = http::HeaderMap::new();
    req.insert(http::header::COOKIE, cookie.split(';').next().unwrap().parse().unwrap());
    let mut resp = http::HeaderMap::new();
    umbral_auth::logout(&req, &mut resp).await.unwrap();
    // logout emits a clearing Set-Cookie.
    assert!(resp.get(http::header::SET_COOKIE).is_some(), "logout sets a clearing cookie");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbral-auth --test logout_fn`
Expected: FAIL — `umbral_auth::logout` not found.

- [ ] **Step 3: Implement** — in `lib.rs`:

```rust
/// Log the current request's user out: destroy the session row and emit a
/// clearing Set-Cookie on `resp`. The single reusable logout — both built-in
/// surfaces and any custom handler call this. Does NOT revoke bearer tokens
/// (those are explicit-revoke; use `AuthToken::revoke`).
pub async fn logout(
    req: &http::HeaderMap,
    resp: &mut http::HeaderMap,
) -> Result<(), AuthError> {
    umbral_sessions::logout(req, resp)
        .await
        .map_err(|e| AuthError::Session(e.to_string()))
}
```

(Use the existing `AuthError` session variant if one exists — grep `enum AuthError` in `lib.rs`; reuse the variant `login_with_request` maps `SessionError` into. If none, add `Session(String)`.)

Update `auth_routes::logout` to call `crate::logout(&headers, response.headers_mut()).await` instead of `umbral_sessions::logout` directly (keep the 204 on success; log on error).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p umbral-auth --test logout_fn`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add plugins/umbral-auth/src/lib.rs plugins/umbral-auth/src/auth_routes.rs plugins/umbral-auth/tests/logout_fn.rs
git commit -m "feat(auth): expose reusable umbral_auth::logout used by both surfaces"
```

---

### Task 8: Email-verification core (`start_email_verification`, `verify_email`)

**Files:**
- Modify: `plugins/umbral-auth/src/challenge.rs` (the two flow fns + a small body-render helper) — or a new `flows.rs`; keep them in `challenge.rs` to colocate with the model.
- Create: `plugins/umbral-auth/templates/auth/email/verify_code.{html,txt}` (minimal; full set in Task 13, but the `.txt` is needed now for rendering).
- Test: `plugins/umbral-auth/tests/verify_email.rs` (new — wires a recording mailer via `AuthPlugin::mailer`, boots an app, drives the flow)

**Interfaces:**
- Produces:
  - `pub async fn start_email_verification(user: &AuthUser) -> Result<(), AuthError>` — issues a code (TTL 15m), renders `auth/email/verify_code.{html,txt}` with `{ code, username }`, sends via `active_mailer()`.
  - `pub async fn verify_email(email: &str, code: &str) -> Result<(), AuthError>` — finds the user by email + active code challenge; on match sets `email_verified_at = now` and marks the challenge used; bumps attempts and invalidates at 5; returns `AuthError::InvalidChallenge` generically on any failure.
- Consumes: `active_mailer()`, `OutgoingMail`, `umbral::templates::render`, `AuthChallenge`, `AuthUser`.

- [ ] **Step 1: Write the failing test**

```rust
// boot_with_recorder() builds an App with AuthPlugin::default().mailer(Recorder)
// (Recorder from Task 5 pattern, stored so the test can read captured mail),
// creates the auth tables, returns (router-or-nothing, recorder handle).
#[tokio::test]
async fn verify_email_happy_path_and_wrong_code() {
    let rec = boot_with_recorder().await;
    let user = umbral_auth::create_user("bob", "bob@example.com", "Sup3r$ecret!").await.unwrap();
    assert!(user.email_verified_at.is_none());

    umbral_auth::start_email_verification(&user).await.unwrap();
    // The recorder captured exactly one mail to bob; extract the 6-digit code.
    let mail = rec.last().expect("a verification email");
    assert_eq!(mail.to, "bob@example.com");
    let code: String = mail.text.chars().filter(|c| c.is_ascii_digit()).collect();
    assert_eq!(code.len(), 6, "email body contains the 6-digit code: {}", mail.text);

    // Wrong code fails generically; correct code verifies.
    assert!(umbral_auth::verify_email("bob@example.com", "000000").await.is_err()
        || code == "000000");
    umbral_auth::verify_email("bob@example.com", &code).await.unwrap();

    let reloaded = umbral_auth::AuthUser::objects()
        .filter(umbral_auth::auth_user::EMAIL.eq("bob@example.com".to_string()))
        .first().await.unwrap().unwrap();
    assert!(reloaded.email_verified_at.is_some(), "email marked verified");

    // Single-use: the same code can't verify twice.
    assert!(umbral_auth::verify_email("bob@example.com", &code).await.is_err());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbral-auth --test verify_email`
Expected: FAIL — flow fns not found.

- [ ] **Step 3: Implement**

Create `templates/auth/email/verify_code.txt`:

```
Hi {{ username }},

Your verification code is: {{ code }}

It expires in 15 minutes. If you didn't request this, ignore this email.
```

Create `templates/auth/email/verify_code.html`:

```html
<p>Hi {{ username }},</p>
<p>Your verification code is: <strong>{{ code }}</strong></p>
<p>It expires in 15 minutes. If you didn't request this, ignore this email.</p>
```

Append to `challenge.rs`:

```rust
use crate::mailer::{active_mailer, OutgoingMail};
use umbral::templates::{context, render};

const CODE_TTL: Duration = Duration::from_secs(15 * 60);
const MAX_CODE_ATTEMPTS: i32 = 5;

pub async fn start_email_verification(user: &crate::AuthUser) -> Result<(), AuthError> {
    let code = generate_code();
    AuthChallenge::issue(user.id, PURPOSE_EMAIL_VERIFY, &code, CODE_TTL).await?;
    let ctx = context! { code => code, username => user.username.clone() };
    let html = render("auth/email/verify_code.html", &ctx).map_err(|e| AuthError::Template(e.to_string()))?;
    let text = render("auth/email/verify_code.txt", &ctx).map_err(|e| AuthError::Template(e.to_string()))?;
    active_mailer()
        .send(OutgoingMail { to: user.email.clone(), subject: "Verify your email".into(), html, text })
        .await
        .map_err(|e| AuthError::Mail(e.to_string()))?;
    Ok(())
}

pub async fn verify_email(email: &str, code: &str) -> Result<(), AuthError> {
    let Some(user) = crate::AuthUser::objects()
        .filter(crate::auth_user::EMAIL.eq(email.to_string()))
        .first()
        .await?
    else {
        return Err(AuthError::InvalidChallenge);
    };
    let Some(challenge) = AuthChallenge::find_active_for_user(user.id, PURPOSE_EMAIL_VERIFY).await? else {
        return Err(AuthError::InvalidChallenge);
    };
    if challenge.attempts >= MAX_CODE_ATTEMPTS {
        challenge.mark_used().await?; // burn it
        return Err(AuthError::InvalidChallenge);
    }
    if hash_secret(code) != challenge.secret_hash {
        challenge.bump_attempts().await?;
        return Err(AuthError::InvalidChallenge);
    }
    challenge.mark_used().await?;
    let mut delta = serde_json::Map::new();
    delta.insert("email_verified_at".to_string(), serde_json::json!(Utc::now()));
    crate::AuthUser::objects()
        .filter(crate::auth_user::ID.eq(user.id))
        .update_values(delta)
        .await?;
    Ok(())
}
```

Re-export both from `lib.rs`: `pub use challenge::{start_email_verification, verify_email};`. Add `AuthError` variants `Template(String)`, `Mail(String)`, `InvalidChallenge` if absent (grep the enum first).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p umbral-auth --test verify_email`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add plugins/umbral-auth/src/challenge.rs plugins/umbral-auth/src/lib.rs plugins/umbral-auth/templates/auth/email/ plugins/umbral-auth/tests/verify_email.rs
git commit -m "feat(auth): email-verification core flow (code issue + verify)"
```

---

### Task 9: Password forgot/reset core (`start_password_reset`, `reset_password`)

**Files:**
- Modify: `plugins/umbral-auth/src/challenge.rs`
- Create: `plugins/umbral-auth/templates/auth/email/reset_link.{html,txt}`
- Test: `plugins/umbral-auth/tests/password_reset.rs` (new)

**Interfaces:**
- Produces:
  - `pub async fn start_password_reset(email: &str, reset_url_base: &str) -> Result<(), AuthError>` — silent no-op on unknown email; issues a token (TTL 1h), renders `auth/email/reset_link.{html,txt}` with `{ reset_url }` where `reset_url = format!("{reset_url_base}?token={token}")`, sends.
  - `pub async fn reset_password(token: &str, new_password: &str) -> Result<(), AuthError>` — validates challenge; runs `crate::validate_password`; sets new hash via `crate::hash_password`; marks used; revokes the user's bearer tokens (`AuthToken`) + sessions (`umbral_sessions::revoke_user_sessions`).
- Consumes: `umbral_sessions::revoke_user_sessions` (Task 3), `crate::validate_password`, `crate::hash_password`, `crate::token::auth_token` consts.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn reset_flow_changes_password_and_revokes_tokens() {
    let rec = boot_with_recorder().await;
    let user = umbral_auth::create_user("carol", "carol@example.com", "Old$Passw0rd").await.unwrap();
    // Give her a bearer token and a session.
    let (_t, _pt) = umbral_auth::token::AuthToken::create_for(&user, "laptop").await.unwrap();
    umbral_sessions::login_user_id(&http::HeaderMap::new(), &mut http::HeaderMap::new(), Some(user.id.to_string())).await.unwrap();

    // Unknown email is a silent success (no enumeration), sends nothing.
    umbral_auth::start_password_reset("nobody@example.com", "https://app/reset").await.unwrap();
    assert!(rec.all().iter().all(|m| m.to != "nobody@example.com"));

    umbral_auth::start_password_reset("carol@example.com", "https://app/reset").await.unwrap();
    let mail = rec.last().expect("reset email");
    // Extract token from the URL in the body.
    let token = mail.text.split("token=").nth(1).unwrap().split_whitespace().next().unwrap().to_string();

    // Weak password is rejected.
    assert!(umbral_auth::reset_password(&token, "123").await.is_err());
    // Strong password succeeds.
    umbral_auth::reset_password(&token, "Br4nd-New$Pass").await.unwrap();

    // New password authenticates; old does not.
    assert!(umbral_auth::authenticate("carol", "Br4nd-New$Pass").await.is_ok());
    assert!(umbral_auth::authenticate("carol", "Old$Passw0rd").await.is_err());

    // Tokens + sessions revoked.
    let tok_count = umbral_auth::token::AuthToken::objects()
        .filter(umbral_auth::token::auth_token::USER_ID.eq(user.id)).count().await.unwrap();
    assert_eq!(tok_count, 0, "bearer tokens revoked on reset");

    // Single-use: token can't be reused.
    assert!(umbral_auth::reset_password(&token, "An0ther$Pass!").await.is_err());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbral-auth --test password_reset`
Expected: FAIL — fns not found.

- [ ] **Step 3: Implement**

`templates/auth/email/reset_link.txt`:

```
Someone requested a password reset for your account.

Reset your password: {{ reset_url }}

This link expires in 1 hour. If you didn't request it, ignore this email.
```

`templates/auth/email/reset_link.html`:

```html
<p>Someone requested a password reset for your account.</p>
<p><a href="{{ reset_url }}">Reset your password</a></p>
<p>This link expires in 1 hour. If you didn't request it, ignore this email.</p>
```

Append to `challenge.rs`:

```rust
const RESET_TTL: Duration = Duration::from_secs(60 * 60);

pub async fn start_password_reset(email: &str, reset_url_base: &str) -> Result<(), AuthError> {
    // Silent on unknown email — never reveal whether an account exists.
    let Some(user) = crate::AuthUser::objects()
        .filter(crate::auth_user::EMAIL.eq(email.to_string()))
        .first()
        .await?
    else {
        return Ok(());
    };
    let token = generate_reset_token();
    AuthChallenge::issue(user.id, PURPOSE_PASSWORD_RESET, &token, RESET_TTL).await?;
    let reset_url = format!("{reset_url_base}?token={token}");
    let ctx = context! { reset_url => reset_url, username => user.username.clone() };
    let html = render("auth/email/reset_link.html", &ctx).map_err(|e| AuthError::Template(e.to_string()))?;
    let text = render("auth/email/reset_link.txt", &ctx).map_err(|e| AuthError::Template(e.to_string()))?;
    active_mailer()
        .send(OutgoingMail { to: user.email.clone(), subject: "Reset your password".into(), html, text })
        .await
        .map_err(|e| AuthError::Mail(e.to_string()))?;
    Ok(())
}

pub async fn reset_password(token: &str, new_password: &str) -> Result<(), AuthError> {
    let Some(challenge) = AuthChallenge::find_active_by_secret(token, PURPOSE_PASSWORD_RESET).await? else {
        return Err(AuthError::InvalidChallenge);
    };
    let user_id: i64 = challenge.user_id.id(); // ForeignKey -> PK
    let Some(user) = crate::AuthUser::objects()
        .filter(crate::auth_user::ID.eq(user_id))
        .first()
        .await?
    else {
        return Err(AuthError::InvalidChallenge);
    };
    // Enforce the strength policy on the new password.
    crate::validate_password(new_password, &crate::PasswordContext::new(Some(&user.username), Some(&user.email)))
        .map_err(|reasons| AuthError::WeakPassword(reasons.join(" ")))?;
    let hash = crate::hash_password(new_password)?;
    let mut delta = serde_json::Map::new();
    delta.insert("password_hash".to_string(), serde_json::json!(hash));
    crate::AuthUser::objects()
        .filter(crate::auth_user::ID.eq(user_id))
        .update_values(delta)
        .await?;
    challenge.mark_used().await?;
    // Log out everywhere: a reset implies possible compromise.
    crate::token::AuthToken::objects()
        .filter(crate::token::auth_token::USER_ID.eq(user_id))
        .delete()
        .await?;
    let _ = umbral_sessions::revoke_user_sessions(&user_id.to_string()).await;
    Ok(())
}
```

(Verify `ForeignKey::id()` accessor name — grep `impl<...> ForeignKey`; it may be `.id()`, `.pk()`, or a field. Adjust. Add `AuthError::WeakPassword(String)` if absent.)

Re-export: `pub use challenge::{start_password_reset, reset_password};`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p umbral-auth --test password_reset`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add plugins/umbral-auth/src/challenge.rs plugins/umbral-auth/src/lib.rs plugins/umbral-auth/templates/auth/email/ plugins/umbral-auth/tests/password_reset.rs
git commit -m "feat(auth): password forgot/reset core (token issue + reset with revoke)"
```

---

### Task 10: JSON surface — new endpoints + base-path resolution

**Files:**
- Modify: `plugins/umbral-auth/src/lib.rs` (a `json_prefix()` helper on the plugin; pass it into `auth_routes`), `plugins/umbral-auth/src/auth_routes.rs` (handlers + router + declared_routes).
- Test: `plugins/umbral-auth/tests/json_surface.rs` (new — boots an app with `with_default_routes()`, drives endpoints through the router with a recording mailer)

**Interfaces:**
- Consumes: `start_email_verification`, `verify_email`, `start_password_reset`, `reset_password`, `umbral::web::api_base`.
- Produces: routes under `{prefix}` where `prefix = self.default_routes_prefix.clone().unwrap_or_else(|| format!("{}/auth", umbral::web::api_base()))`:
  - `POST {prefix}/verify-email` `{email, code}` → 204 / 400 generic
  - `POST {prefix}/resend-verification` `{email}` → 202 always
  - `POST {prefix}/password-forgot` `{email}` → 202 always
  - `POST {prefix}/password-reset` `{token, new_password}` → 204 / 400

- [ ] **Step 1: Write the failing test**

```rust
async fn post(router: &axum::Router, uri: &str, body: &str) -> axum::http::StatusCode {
    use tower::ServiceExt;
    let req = axum::http::Request::builder().method("POST").uri(uri)
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body.to_string())).unwrap();
    router.clone().oneshot(req).await.unwrap().status()
}

#[tokio::test]
async fn json_verify_and_reset_endpoints() {
    let (router, rec) = boot_app_with_recorder().await; // with_default_routes() + mailer(Recorder)
    // Register via the JSON route.
    assert_eq!(post(&router, "/api/auth/register",
        r#"{"username":"dan","email":"dan@example.com","password":"G00d$Pass!"}"#).await,
        axum::http::StatusCode::CREATED);

    // Resend verification: always 202, generic.
    assert_eq!(post(&router, "/api/auth/resend-verification", r#"{"email":"dan@example.com"}"#).await,
        axum::http::StatusCode::ACCEPTED);
    let code: String = rec.last().unwrap().text.chars().filter(|c| c.is_ascii_digit()).collect();

    // Wrong code → 400 generic; right code → 204.
    assert_eq!(post(&router, "/api/auth/verify-email", r#"{"email":"dan@example.com","code":"000000"}"#).await,
        axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(post(&router, "/api/auth/verify-email",
        &format!(r#"{{"email":"dan@example.com","code":"{code}"}}"#)).await,
        axum::http::StatusCode::NO_CONTENT);

    // Forgot is always 202 even for unknown emails.
    assert_eq!(post(&router, "/api/auth/password-forgot", r#"{"email":"ghost@example.com"}"#).await,
        axum::http::StatusCode::ACCEPTED);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbral-auth --test json_surface`
Expected: FAIL — routes 404 (not mounted).

- [ ] **Step 3: Implement**

In `lib.rs`, add a helper used by `routes()`, `route_paths()`, `openapi_paths()`:

```rust
impl AuthPlugin<AuthUser> {
    /// The JSON auth prefix: an explicit `with_default_routes_at` override,
    /// else `{api_base}/auth` (the REST base path, "/api" by default).
    fn json_prefix(&self) -> Option<String> {
        self.default_routes_prefix.as_ref().map(|p| {
            if p == JSON_PREFIX_SENTINEL { format!("{}/auth", umbral::web::api_base()) } else { p.clone() }
        })
    }
}
```

Change `with_default_routes()` to store a sentinel rather than the literal `/api/auth`:

```rust
const JSON_PREFIX_SENTINEL: &str = "\0auto-api-base\0";
// with_default_routes(): self.default_routes_prefix = Some(JSON_PREFIX_SENTINEL.to_string());
// with_default_routes_at(p): unchanged (stores the explicit prefix).
```

Update `routes()`, `route_paths()`, `openapi_paths()` to use `self.json_prefix()` instead of `&self.default_routes_prefix`.

In `auth_routes.rs`, add DTOs + handlers and extend `build_router`/`declared_routes`:

```rust
#[derive(Debug, Deserialize)] struct VerifyEmailIn { email: String, code: String }
#[derive(Debug, Deserialize)] struct EmailOnlyIn { email: String }
#[derive(Debug, Deserialize)] struct ResetIn { token: String, new_password: String }

async fn verify_email_h(Json(b): Json<VerifyEmailIn>) -> Response {
    match crate::verify_email(&b.email, &b.code).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(_) => err(StatusCode::BAD_REQUEST, "invalid_code", "verification failed"),
    }
}
async fn resend_verification_h(Json(b): Json<EmailOnlyIn>) -> Response {
    // Generic 202 regardless of existence/verified state (no enumeration).
    if let Ok(Some(u)) = AuthUser::objects()
        .filter(auth_user::EMAIL.eq(b.email.clone()) & auth_user::EMAIL_VERIFIED_AT.is_null())
        .first().await {
        let _ = crate::start_email_verification(&u).await;
    }
    StatusCode::ACCEPTED.into_response()
}
async fn password_forgot_h(headers: HeaderMap, Json(b): Json<EmailOnlyIn>) -> Response {
    let base = reset_url_base(&headers); // {scheme}://{host}{prefix}/reset or a configured value
    let _ = crate::start_password_reset(&b.email, &base).await;
    StatusCode::ACCEPTED.into_response()
}
async fn password_reset_h(Json(b): Json<ResetIn>) -> Response {
    match crate::reset_password(&b.token, &b.new_password).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(_) => err(StatusCode::BAD_REQUEST, "reset_failed", "could not reset password"),
    }
}
```

Add a `reset_url_base(&HeaderMap) -> String` helper: prefer `Host` + `X-Forwarded-Proto` to build `{proto}://{host}/auth/reset`; fall back to `"/auth/reset"`. (The HTML surface in Task 14 owns the canonical `/auth/reset` page; the JSON forgot endpoint points the email there.)

Extend `build_router`:

```rust
.route(&format!("{prefix}/verify-email"), post(verify_email_h))
.route(&format!("{prefix}/resend-verification"), post(resend_verification_h))
.route(&format!("{prefix}/password-forgot"), post(password_forgot_h))
.route(&format!("{prefix}/password-reset"), post(password_reset_h))
```

and add the matching `("POST", format!("{prefix}/..."))` entries to `declared_routes`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p umbral-auth --test json_surface`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add plugins/umbral-auth/src/lib.rs plugins/umbral-auth/src/auth_routes.rs plugins/umbral-auth/tests/json_surface.rs
git commit -m "feat(auth): JSON verify/resend/forgot/reset endpoints under the REST base path"
```

---

### Task 11: OpenAPI entries for the new JSON endpoints

**Files:**
- Modify: `plugins/umbral-auth/src/auth_routes.rs` (`openapi_paths`)
- Test: `plugins/umbral-auth/tests/json_surface.rs` (extend) or a focused unit test in `auth_routes.rs`.

**Interfaces:**
- Consumes: `openapi_paths(prefix)`.
- Produces: Path Item entries for `verify-email`, `resend-verification`, `password-forgot`, `password-reset` under the `auth` tag.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn openapi_lists_the_new_auth_endpoints() {
    let paths = umbral_auth::auth_routes_openapi_for_test("/api/auth"); // thin #[doc(hidden)] pub shim over openapi_paths
    let keys: Vec<&str> = paths.iter().map(|(p, _)| p.as_str()).collect();
    for p in ["/api/auth/verify-email", "/api/auth/password-reset", "/api/auth/password-forgot", "/api/auth/resend-verification"] {
        assert!(keys.contains(&p), "openapi missing {p}; got {keys:?}");
    }
}
```

(Add `#[doc(hidden)] pub fn auth_routes_openapi_for_test(prefix: &str) -> Vec<(String, serde_json::Value)> { auth_routes::openapi_paths(prefix) }` in `lib.rs`, mirroring the existing test shims, OR call through a real built app's `openapi_paths`.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbral-auth openapi_lists_the_new_auth_endpoints`
Expected: FAIL — keys missing.

- [ ] **Step 3: Implement** — append four entries to the `vec![...]` in `openapi_paths`, mirroring the existing `register`/`login` shapes. Example for verify-email:

```rust
(
    format!("{prefix}/verify-email"),
    json!({ "post": {
        "tags": [tag], "operationId": "auth_verify_email",
        "summary": "Verify an email address with a 6-digit code.",
        "requestBody": { "required": true, "content": { "application/json": { "schema": json!({
            "type":"object","required":["email","code"],
            "properties":{"email":{"type":"string","format":"email"},"code":{"type":"string","example":"483920"}}
        })}}},
        "responses": { "204": {"description":"Verified."}, "400": {"description":"Invalid or expired code.", "content":{"application/json":{"schema": error_response.clone()}}} }
    }}),
),
```

Add analogous entries for `resend-verification` (202), `password-forgot` (202), `password-reset` (204 / 400 with `{token, new_password}`).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p umbral-auth openapi_lists_the_new_auth_endpoints`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add plugins/umbral-auth/src/auth_routes.rs plugins/umbral-auth/src/lib.rs plugins/umbral-auth/tests/json_surface.rs
git commit -m "feat(auth): OpenAPI path items for verify/resend/forgot/reset"
```

---

### Task 12: `require_verified_email` enforcement

**Files:**
- Modify: `plugins/umbral-auth/src/lib.rs` (builder + flag + ambient), `plugins/umbral-auth/src/auth_routes.rs` (register auto-sends; login blocks).
- Test: `plugins/umbral-auth/tests/require_verified.rs` (new)

**Interfaces:**
- Produces: `AuthPlugin::require_verified_email() -> Self`; an ambient `verified_email_required() -> bool` read by handlers; behavior: with it on, `register` (JSON) auto-calls `start_email_verification`, and `login` returns `403 {error:"email_not_verified"}` while `email_verified_at IS NULL`.
- Consumes: `start_email_verification`.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn unverified_login_blocked_only_when_required() {
    // App A: require_verified_email() ON.
    let (router, _rec) = boot_app_required().await;
    post(&router, "/api/auth/register", r#"{"username":"e","email":"e@x.com","password":"G00d$Pass!"}"#).await;
    let status = post(&router, "/api/auth/login", r#"{"username":"e","password":"G00d$Pass!"}"#).await;
    assert_eq!(status, axum::http::StatusCode::FORBIDDEN, "unverified login blocked when required");

    // App B (separate test binary or separate ambient): default — login allowed unverified.
    // (Implement as a second test fn in its own file if the ambient flag is process-global.)
}
```

(Because the flag is sealed into a process-global `OnceLock` in `on_ready`, put the "required" and "not required" cases in **separate test files** so each builds one app.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbral-auth --test require_verified`
Expected: FAIL — builder/flag absent, login returns 200.

- [ ] **Step 3: Implement**

In `lib.rs`: add field `require_verified: bool` (default false), builder:

```rust
/// Block login until the user's email is verified, and auto-send a
/// verification code on register. Off by default (the endpoints exist and
/// `email_verified_at` is tracked regardless). Requires a working mailer in
/// production — see `AuthPlugin::mailer`.
pub fn require_verified_email(mut self) -> Self { self.require_verified = true; self }
```

Add an ambient seal (mirror the policy/mailer pattern):

```rust
static REQUIRE_VERIFIED: OnceLock<bool> = OnceLock::new();
pub(crate) fn verified_email_required() -> bool { *REQUIRE_VERIFIED.get().unwrap_or(&false) }
// in on_ready: let _ = REQUIRE_VERIFIED.set(self.require_verified);
```

In `auth_routes.rs`:
- In `register`, after a successful `create_user`, if `crate::verified_email_required()` then `let _ = crate::start_email_verification(&user).await;` (best-effort; failure to mail doesn't fail registration, but log it).
- In `login`, after `authenticate` succeeds and before minting the token, if `crate::verified_email_required() && user.email_verified_at.is_none()` return `err(StatusCode::FORBIDDEN, "email_not_verified", "verify your email before logging in")`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p umbral-auth --test require_verified`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add plugins/umbral-auth/src/lib.rs plugins/umbral-auth/src/auth_routes.rs plugins/umbral-auth/tests/require_verified.rs
git commit -m "feat(auth): opt-in require_verified_email (auto-send on register, block login)"
```

---

### Task 13: Jinja templates

> **SUPERSEDED 2026-06-29 (reverted in commit 4d0e0c9).** The framework no longer ships login/signup/verify/reset *pages* — those carry the developer's brand and are theirs to write. The page templates created here were reverted. Email-body templates (shipped in Tasks 8–9) stay. See the revised "Form-action endpoints" section in `docs/decisions/2026-06-28-auth-full-surface.md`. The original content below is retained for history only.

**Files:**
- Create: `plugins/umbral-auth/templates/auth/base.html`, `login.html`, `signup.html`, `verify.html`, `forgot.html`, `reset.html`.
- (Email templates already exist from Tasks 8–9.)
- Test: none in isolation — exercised by Task 14's handler tests.

**Interfaces:**
- Produces: templates rendered by Task 14 handlers. Context keys each page expects are listed inline.

- [ ] **Step 1: Create `base.html`** (overridable shell; apps replace this one file to theme all pages):

```html
<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>{% block title %}Account{% endblock %}</title></head>
<body>
  {% if messages %}<ul class="messages">
    {% for m in messages %}<li class="msg msg-{{ m.level }}">{{ m.text }}</li>{% endfor %}
  </ul>{% endif %}
  <main>{% block content %}{% endblock %}</main>
</body>
</html>
```

- [ ] **Step 2: Create the page templates**

`login.html` (context: `next`, `error`):

```html
{% extends "auth/base.html" %}
{% block title %}Sign in{% endblock %}
{% block content %}
<h1>Sign in</h1>
<form method="post">
  {{ csrf_input | safe }}
  <input type="hidden" name="next" value="{{ next | default('') }}">
  <label>Username <input name="username" required></label>
  <label>Password <input name="password" type="password" required></label>
  <button type="submit">Sign in</button>
</form>
<p><a href="forgot">Forgot password?</a> · <a href="signup">Create account</a></p>
{% endblock %}
```

`signup.html` (context: none required):

```html
{% extends "auth/base.html" %}
{% block title %}Create account{% endblock %}
{% block content %}
<h1>Create account</h1>
<form method="post">
  {{ csrf_input | safe }}
  <label>Username <input name="username" required></label>
  <label>Email <input name="email" type="email" required></label>
  <label>Password <input name="password" type="password" required></label>
  <button type="submit">Sign up</button>
</form>
<p><a href="login">Already have an account? Sign in</a></p>
{% endblock %}
```

`verify.html` (context: `email`):

```html
{% extends "auth/base.html" %}
{% block title %}Verify email{% endblock %}
{% block content %}
<h1>Verify your email</h1>
<form method="post">
  {{ csrf_input | safe }}
  <input type="hidden" name="email" value="{{ email | default('') }}">
  <label>Enter the 6-digit code <input name="code" inputmode="numeric" pattern="[0-9]{6}" required></label>
  <button type="submit">Verify</button>
</form>
<form method="post" action="resend">
  {{ csrf_input | safe }}
  <input type="hidden" name="email" value="{{ email | default('') }}">
  <button type="submit">Resend code</button>
</form>
{% endblock %}
```

`forgot.html`:

```html
{% extends "auth/base.html" %}
{% block title %}Forgot password{% endblock %}
{% block content %}
<h1>Reset your password</h1>
<form method="post">
  {{ csrf_input | safe }}
  <label>Email <input name="email" type="email" required></label>
  <button type="submit">Send reset link</button>
</form>
{% endblock %}
```

`reset.html` (context: `token`):

```html
{% extends "auth/base.html" %}
{% block title %}Set a new password{% endblock %}
{% block content %}
<h1>Set a new password</h1>
<form method="post">
  {{ csrf_input | safe }}
  <input type="hidden" name="token" value="{{ token }}">
  <label>New password <input name="new_password" type="password" required></label>
  <button type="submit">Set password</button>
</form>
{% endblock %}
```

- [ ] **Step 3: Verify they parse** — covered when Task 14 renders them. No standalone run.

- [ ] **Step 4: Commit**

```bash
git add plugins/umbral-auth/templates/auth/*.html
git commit -m "feat(auth): ship overridable Jinja templates for the auth pages"
```

---

### Task 14: HTML surface — `template_routes` + `with_template_pages`

> **SUPERSEDED 2026-06-29.** Redesigned as POST-only **form-action endpoints** (form in → 303 redirect out; flash errors; `?redirect=` + Referer-fallback, open-redirect-safe). No GET pages, no shipped page templates. The authoritative requirements are in `.superpowers/sdd/task-14-brief.md` and the revised "Form-action endpoints (`with_form_routes()`)" section of `docs/decisions/2026-06-28-auth-full-surface.md`. The original full-page content below is retained for history only — do NOT implement it.

**Files:**
- Create: `plugins/umbral-auth/src/template_routes.rs`
- Modify: `plugins/umbral-auth/src/lib.rs` (`pub mod template_routes;`; `template_pages_prefix` field; `with_template_pages[_at]` builders; `routes()` merges the page router; `templates_dirs()` returns the shipped dir; `route_paths()` adds the pages).
- Test: `plugins/umbral-auth/tests/template_surface.rs` (new)

**Interfaces:**
- Consumes: core flow fns, `umbral_sessions::messages::Messages`, `umbral::templates::render`, `umbral::web::{Html, Form, Redirect}`.
- Produces: `with_template_pages() -> Self` (default prefix `/auth`), `with_template_pages_at(prefix) -> Self`; GET/POST handlers per the spec table.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn template_pages_render_and_login_redirects() {
    let (router, _rec) = boot_template_app().await; // with_template_pages().with_user_in_templates()
    // GET login renders a form (200, contains the form).
    use tower::ServiceExt;
    let resp = router.clone().oneshot(
        axum::http::Request::builder().method("GET").uri("/auth/login")
            .body(axum::body::Body::empty()).unwrap()).await.unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    assert!(String::from_utf8_lossy(&body).contains("name=\"username\""), "login form rendered");

    // POST signup creates the user and redirects (303/302).
    let resp = post_form(&router, "/auth/signup",
        "username=fred&email=fred@x.com&password=G00d%24Pass%21").await;
    assert!(resp.is_redirection(), "signup redirects after success; got {resp}");
    assert!(umbral_auth::authenticate("fred", "G00d$Pass!").await.is_ok());
}
```

(`post_form` builds an `application/x-www-form-urlencoded` POST. For CSRF: if the automatic-CSRF middleware is active in the test app, either disable it for the test app build or include a valid token — check how other template-POST tests in the repo handle CSRF, e.g. the admin tests, and mirror that.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbral-auth --test template_surface`
Expected: FAIL — `/auth/login` 404.

- [ ] **Step 3: Implement** — `template_routes.rs`:

```rust
//! Server-rendered (MiniJinja) auth pages. Thin: parse form → call a core
//! fn → flash + redirect (PRG). Templates live in ../templates/auth and are
//! overridable by the app.

use crate::{AuthUser, auth_user};
use serde::Deserialize;
use umbral::templates::{context, render};
use umbral::web::{Form, Html, HeaderMap, IntoResponse, Redirect, Response, Router, get, post};
use umbral_sessions::messages::Messages;

fn page(name: &str, ctx: minijinja::Value) -> Response {
    match render(name, &ctx) {
        Ok(b) => Html(b).into_response(),
        Err(e) => {
            tracing::error!(error = %e, template = name, "auth page render failed");
            (umbral::web::StatusCode::INTERNAL_SERVER_ERROR, "template error").into_response()
        }
    }
}

#[derive(Deserialize)] struct LoginForm { username: String, password: String, #[serde(default)] next: String }
#[derive(Deserialize)] struct SignupForm { username: String, email: String, password: String }
#[derive(Deserialize)] struct VerifyForm { email: String, code: String }
#[derive(Deserialize)] struct EmailForm { email: String }
#[derive(Deserialize)] struct ResetForm { token: String, new_password: String }
#[derive(Deserialize)] struct TokenQuery { #[serde(default)] token: String, #[serde(default)] email: String }

pub(crate) fn build_router(prefix: &str) -> Router {
    Router::new()
        .route(&format!("{prefix}/login"), get(login_page).post(do_login))
        .route(&format!("{prefix}/signup"), get(signup_page).post(do_signup))
        .route(&format!("{prefix}/logout"), post(do_logout))
        .route(&format!("{prefix}/verify"), get(verify_page).post(do_verify))
        .route(&format!("{prefix}/resend"), post(do_resend))
        .route(&format!("{prefix}/forgot"), get(forgot_page).post(do_forgot))
        .route(&format!("{prefix}/reset"), get(reset_page).post(do_reset))
}

pub(crate) fn declared_routes(prefix: &str) -> Vec<umbral::routes::RouteSpec> {
    ["login","signup","logout","verify","resend","forgot","reset"]
        .into_iter().flat_map(|p| {
            let path = format!("{prefix}/{p}");
            // GET + POST for the page pairs; logout/resend are POST-only.
            if p == "logout" || p == "resend" { vec![("POST", path).into()] }
            else { vec![("GET", path.clone()).into(), ("POST", path).into()] }
        }).collect()
}

async fn login_page(q: umbral::web::Query<TokenQuery>) -> Response {
    page("auth/login.html", context! { next => q.0.email /* unused */ , error => false })
}
async fn do_login(headers: HeaderMap, msgs: Messages, Form(f): Form<LoginForm>) -> Response {
    match crate::authenticate(&f.username, &f.password).await {
        Ok(user) => {
            if crate::verified_email_required() && user.email_verified_at.is_none() {
                msgs.error("Verify your email before signing in.").await;
                return Redirect::to(&format!("verify?email={}", urlencoding::encode(&user.email))).into_response();
            }
            let mut resp = Redirect::to(if f.next.is_empty() { "/" } else { &f.next }).into_response();
            let _ = crate::login_with_request(&headers, resp.headers_mut(), &user).await;
            resp
        }
        Err(_) => { msgs.error("Invalid username or password.").await; Redirect::to("login").into_response() }
    }
}
async fn signup_page() -> Response { page("auth/signup.html", context! {}) }
async fn do_signup(msgs: Messages, Form(f): Form<SignupForm>) -> Response {
    match crate::create_user(&f.username, &f.email, &f.password).await {
        Ok(user) => {
            if crate::verified_email_required() {
                let _ = crate::start_email_verification(&user).await;
                msgs.success("Account created. Check your email for a verification code.").await;
                Redirect::to(&format!("verify?email={}", urlencoding::encode(&user.email))).into_response()
            } else {
                msgs.success("Account created. You can sign in now.").await;
                Redirect::to("login").into_response()
            }
        }
        Err(e) => { msgs.error(&format!("Could not create account: {e}")).await; Redirect::to("signup").into_response() }
    }
}
async fn do_logout(headers: HeaderMap) -> Response {
    let mut resp = Redirect::to("login").into_response();
    let _ = crate::logout(&headers, resp.headers_mut()).await;
    resp
}
async fn verify_page(q: umbral::web::Query<TokenQuery>) -> Response {
    page("auth/verify.html", context! { email => q.0.email })
}
async fn do_verify(msgs: Messages, Form(f): Form<VerifyForm>) -> Response {
    match crate::verify_email(&f.email, &f.code).await {
        Ok(()) => { msgs.success("Email verified. You can sign in now.").await; Redirect::to("login").into_response() }
        Err(_) => { msgs.error("Invalid or expired code.").await;
            Redirect::to(&format!("verify?email={}", urlencoding::encode(&f.email))).into_response() }
    }
}
async fn do_resend(msgs: Messages, Form(f): Form<EmailForm>) -> Response {
    if let Ok(Some(u)) = AuthUser::objects()
        .filter(auth_user::EMAIL.eq(f.email.clone()) & auth_user::EMAIL_VERIFIED_AT.is_null()).first().await {
        let _ = crate::start_email_verification(&u).await;
    }
    msgs.info("If that account needs verification, a new code is on its way.").await;
    Redirect::to(&format!("verify?email={}", urlencoding::encode(&f.email))).into_response()
}
async fn forgot_page() -> Response { page("auth/forgot.html", context! {}) }
async fn do_forgot(headers: HeaderMap, msgs: Messages, Form(f): Form<EmailForm>) -> Response {
    let base = crate::auth_routes::reset_url_base(&headers);
    let _ = crate::start_password_reset(&f.email, &base).await;
    msgs.info("If that email has an account, a reset link is on its way.").await;
    Redirect::to("login").into_response()
}
async fn reset_page(q: umbral::web::Query<TokenQuery>) -> Response {
    page("auth/reset.html", context! { token => q.0.token })
}
async fn do_reset(msgs: Messages, Form(f): Form<ResetForm>) -> Response {
    match crate::reset_password(&f.token, &f.new_password).await {
        Ok(()) => { msgs.success("Password updated. Sign in with your new password.").await; Redirect::to("login").into_response() }
        Err(_) => { msgs.error("That reset link is invalid or expired.").await; Redirect::to("forgot").into_response() }
    }
}
```

(Notes for the implementer: confirm `umbral::web` re-exports `Form`, `Query`, `Redirect`, `Html`, `get`, `post`, `StatusCode`. `Messages` method names (`success`/`error`/`info`) and `.await` must match `umbral_sessions::messages::Messages` — adjust to the real API. `reset_url_base` must be made `pub(crate)` in `auth_routes.rs`. `urlencoding` is not a current dep — either add it to Cargo.toml, or inline a tiny percent-encode for the email/token query values, or use `form_urlencoded::byte_serialize`. Prefer reusing whatever the repo already has; if nothing, add `urlencoding = "2"`.)

In `lib.rs`: add field `template_pages_prefix: Option<String>` (default None), builders:

```rust
impl AuthPlugin<AuthUser> {
    /// Mount the server-rendered Jinja auth pages (login/signup/logout/
    /// verify/forgot/reset) at `/auth`. Pairs with `with_user_in_templates()`
    /// so the pages and your app see `{{ user }}`.
    pub fn with_template_pages(mut self) -> Self { self.template_pages_prefix = Some("/auth".into()); self }
    /// Same, with a custom prefix.
    pub fn with_template_pages_at(mut self, p: impl Into<String>) -> Self { self.template_pages_prefix = Some(p.into()); self }
}
```

Extend `routes()` to merge both routers:

```rust
fn routes(&self) -> umbral::web::Router {
    let mut r = umbral::web::Router::new();
    if let Some(prefix) = self.json_prefix() { r = r.merge(auth_routes::build_router(&prefix)); }
    if let Some(prefix) = &self.template_pages_prefix { r = r.merge(template_routes::build_router(prefix)); }
    r
}
```

Add `templates_dirs()`:

```rust
fn templates_dirs(&self) -> Vec<std::path::PathBuf> {
    if self.template_pages_prefix.is_some() {
        vec![std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates")]
    } else { Vec::new() }
}
```

Extend `route_paths()` to also include `template_routes::declared_routes(prefix)` when set.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p umbral-auth --test template_surface`
Expected: PASS. Then `cargo test -p umbral-auth` (whole crate) green.

- [ ] **Step 5: Commit**

```bash
git add plugins/umbral-auth/src/template_routes.rs plugins/umbral-auth/src/lib.rs plugins/umbral-auth/Cargo.toml plugins/umbral-auth/tests/template_surface.rs
git commit -m "feat(auth): server-rendered Jinja auth pages via with_template_pages()"
```

---

### Task 15: Documentation

**Files:**
- Create: `documentation/docs/v0.0.1/auth/email-verification.mdx`, `password-reset.mdx`, `auth-pages.mdx`, `mailer.mdx`.
- Modify: `documentation/docs/v0.0.1/auth/_category_.json` if needed (ensure the `auth` area exists with a category label).

**Interfaces:** none (docs).

- [ ] **Step 1: Confirm the area exists**

Run: `ls documentation/docs/v0.0.1/auth/ 2>/dev/null || echo MISSING`
If MISSING, create `_category_.json`:

```json
{ "label": "Auth", "position": 7, "collapsed": true }
```

- [ ] **Step 2: Write `mailer.mdx`** (purpose, one example, link to the design note):

````mdx
---
title: Auth mailer
description: Wire email sending for the verification and password-reset flows via AuthPlugin::mailer.
sidebar_position: 1
tags: [auth, email]
---

umbral-auth renders email bodies itself and hands them to whatever mailer you wire. With nothing wired it prints to stderr in development (the `ConsoleMailer`), so the flows work with zero config locally.

<Callout type="info">umbral-auth does not depend on umbral-email — pass any sender you like. The one-liner below delegates to the umbral-email plugin if you use it.</Callout>

```rust
use umbral_auth::{AuthPlugin, AuthUser, OutgoingMail, AuthMailError};

AuthPlugin::<AuthUser>::default()
    .with_default_routes()
    .mailer(|m: OutgoingMail| async move {
        umbral_email::send(&umbral_email::EmailMessage::new(m.subject, vec![m.to])
            .html_body(m.html).text_body(m.text)).await
            .map(|_| ()).map_err(|e| AuthMailError::Send(e.to_string()))
    });
```

See the design note `docs/decisions/2026-06-28-auth-full-surface.md`.
````

- [ ] **Step 3: Write `email-verification.mdx`, `password-reset.mdx`, `auth-pages.mdx`**

Each: a Purpose paragraph, one minimal code example, and a link to the design note. Content:
- `email-verification.mdx` — the `verify-email`/`resend-verification` endpoints + `require_verified_email()` + the 6-digit/15-min/5-attempt rules. Example: `AuthPlugin::default().with_default_routes().require_verified_email()`.
- `password-reset.mdx` — `password-forgot`/`password-reset` endpoints, tokenized link (1h, single-use), and that reset revokes sessions + bearer tokens.
- `auth-pages.mdx` — `with_template_pages()`, the route list, and how to override a template (drop `templates/auth/login.html` in your app).

- [ ] **Step 4: Commit**

```bash
git add documentation/docs/v0.0.1/auth/
git commit -m "docs(auth): pages for verification, password reset, auth pages, mailer"
```

---

## Self-review

**Spec coverage:**
- `email_verified_at` + `AuthChallenge` → Task 4. ✓
- Hybrid tokens (code/link, hashed, single-use, TTL, attempts) → Tasks 6, 8, 9. ✓
- Shared core (`logout`, verify, reset) → Tasks 7, 8, 9. ✓
- Pluggable mailer + ConsoleMailer + closure wiring → Task 5; umbral-email one-liner → Task 15. ✓
- JSON surface + base-path auto-follow → Tasks 1, 2, 10; OpenAPI → Task 11. ✓
- HTML surface + templates + override → Tasks 13, 14. ✓
- Opt-in enforcement → Task 12. ✓
- Reset revokes sessions + tokens → Task 3 (sessions helper) + Task 9. ✓
- Enumeration-safe responses → Tasks 10, 14. ✓
- Tests behavioral on both surfaces → Tasks 8–14. ✓
- Docs → Task 15. ✓

**Placeholder scan:** No "TBD"/"implement later". The `(verify against…)` notes are pointers to confirm a real API name before writing, not deferred work — each names the exact file to check and the fallback.

**Type consistency:** `OutgoingMail`/`AuthMailer`/`AuthMailError` consistent across Tasks 5/8/9/15. `AuthChallenge` method names (`issue`/`find_active_for_user`/`find_active_by_secret`/`mark_used`/`bump_attempts`/`is_live`) consistent across Tasks 6/8/9. `json_prefix`/`JSON_PREFIX_SENTINEL` consistent across Tasks 10/11/14. Core flow fn names (`start_email_verification`, `verify_email`, `start_password_reset`, `reset_password`, `logout`) consistent across Tasks 7–14. `verified_email_required()` consistent across Tasks 12/14.

**Known confirm-before-coding points** (each has an inline fallback): `ModelMeta` field accessors (Task 4); `QuerySet::delete` return type (Task 3); `is_null()`/`order_by`/`.desc()` availability (Task 6); `ForeignKey` PK accessor (Task 9); `umbral::web` re-exports of `Form`/`Query`/`Redirect`/`Html` (Task 14); `Messages` method names (Task 14); CSRF handling in template-POST tests (Task 14); whether `urlencoding`/`form_urlencoded` is already available (Task 14).
