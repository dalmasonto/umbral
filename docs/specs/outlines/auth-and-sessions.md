# Outline — Auth and sessions

| | |
|---|---|
| **Status** | Outline. Promotes to a deep spec at M8 entry. |
| **Maps to milestone** | M8 |
| **Companions** | `02-plugin-contract.md`, `04-orm-model-and-fields.md`, `05-backends-and-system-check.md`, outline `web-layer.md`, outline `email.md`, outline `forms.md`, outline `security-defaults.md` |

## Purpose

`umbral-auth` and `umbral-sessions` are the two built-in plugins that turn a stateless HTTP layer into an authenticated, session-bearing web app. Auth owns the `User` model, the password-hashing primitive (argon2), the pluggable authentication-backend chain, permissions, groups, and the login/logout/password-reset flows. Sessions owns the cookie ↔ server-side store bridge - a thin wrapper around `tower-sessions` plus a DB-backed store whose schema is a normal plugin migration. They ship in the box because every non-trivial web app needs both on day one, and because together they are the pressure test that proves the plugin contract: if these two can't be expressed as plugins that depend on each other through `dependencies()` and on no internal core APIs, the contract from `02-plugin-contract.md` is wrong.

## Key concepts

### `User` model and the custom-user-model swap

The default `User` is a normal `#[derive(Model)]` struct shipped by `umbral-auth` — username, email, hashed password, `is_active`, `is_staff`, `is_superuser`, `last_login`, `date_joined`. Apps that need a different shape (email-as-username, extra profile columns) swap it. Resolving the active user model by string name at startup doesn't translate to a statically-typed language; the direction here is an **associated type on a `UserProvider` trait** that `umbral-auth` declares, with the default `User` as one impl and any user-defined struct that satisfies `Model + UserProvider` as another. Other plugins (`umbral-admin`, the `Auth<U>` extractor in `web-layer.md`) reach the active user type through `Plugin::Settings`-style associated type plumbing — never by name — so a swap is a generic parameter change, caught at compile time.

```rust
pub trait UserProvider {
    type User: Model + Authenticatable;
    fn active_model(&self) -> PhantomData<Self::User>;
}
```

The concrete mechanism — associated type vs. a marker trait registered through `App::builder().auth_user::<MyUser>()` — is the deep spec's job to lock down. Open question #5 resolves there.

### Permissions, groups, authentication backends

Permissions are `(codename, model)` pairs auto-generated per model from `FIELDS` metadata (`add_post`, `change_post`, `delete_post`, `view_post`), with custom permissions declarable in the model `Meta`. Groups bundle permissions; users belong to groups and/or hold direct permissions; the resolved set is cached on the `Auth<U>` extractor for the request's lifetime. Authentication backends are a `Vec<Box<dyn AuthBackend>>` walked in order — `ModelBackend` (username+password against the user table) ships by default; LDAP, OAuth, and remote-header backends are third-party plugins that inject themselves into the chain via `umbral-auth`'s settings struct.

### Login / logout and the `Auth<U>` extractor

`login(req, user)` writes the user's PK into the active session; `logout(req)` clears it. The `Auth<U>` extractor reads the session, hydrates the user through the backend chain, and either yields `Auth { user, perms }` or short-circuits the handler. A `#[login_required]` attribute (or the equivalent `LoginRequired` layer) is a thin wrapper that calls the extractor and returns 401/302 on miss.

```rust
async fn create(auth: Auth<User>, Json(p): Json<NewPost>) -> Result<Json<Post>> {
    auth.require_perm("blog.add_post")?;
    Ok(Json(Post::objects().create(p).await?))
}
```

### Password hashing, validators, reset

Hashing is `argon2` with framework-chosen parameters; the hash format is self-describing so parameter upgrades are transparent. Password validators are a small composable catalogue (`MinLength`, `CommonPasswordList`, `NumericOnly`, `UserAttributeSimilarity`) configured in `AuthSettings`, shared with `forms.md`'s password-change form and surfaced as system-check warnings when the list is empty in production. Password reset issues a signed, time-bounded token, delivers it through `umbral-email` (cross-link: outline `email.md`), and verifies on redemption. The auth plugin declares `dependencies() = &["sessions", "email"]` once reset lands.

### Session storage

`umbral-sessions` wraps `tower-sessions` and ships a `DbSessionStore` whose `sessions` table is a normal plugin migration (`session_key`, `data`, `expires_at`, indexed). Cookies are `Secure`, `HttpOnly`, `SameSite=Lax` by default (`security-defaults.md` owns the secure-cookie contract). A cookie-only signed-store backend is available for low-traffic apps that don't want the DB round-trip.

## Promote-to-deep trigger

Promotes at **M8 entry**, when auth and sessions are re-expressed as plugins atop the now-extracted `Plugin` trait. The deep spec locks the custom-user-model mechanism and the full backend trait shape.

## Open questions

- **Custom-user-model mechanism.** Associated type on a `UserProvider` plugin trait vs. a builder-side type parameter (`App::builder().auth_user::<MyUser>()`) vs. a generic `AuthPlugin<U>`. Resolves open question #5 from spec-set design §10.
- **Password validator catalogue.** Which validators ship in the box, which live in `security-defaults.md`, and what the configuration surface looks like (an ordered `Vec<Box<dyn PasswordValidator>>` vs. an enum of built-ins plus an escape hatch).
- **Password reset coupling with email.** Whether `umbral-auth` hard-depends on `umbral-email` (and refuses to boot without it once reset is enabled) or treats email as an optional capability registered through a trait — affects what minimal apps without outbound mail look like.
- **Session garbage collection.** Periodic cleanup of expired rows: a `umbral-tasks` periodic job (creates a soft dependency on the task queue), an `on_ready`-spawned tokio interval, or a `clearsessions` management command run from cron. Each has different deployment ergonomics.
- **Authentication-backend trait async-ness.** Backends touch the DB, so they're async. Whether the trait is `async-trait`-shaped or sidesteps the macro the way `Plugin::on_ready` does in `02-plugin-contract.md`.

## Cross-links

- Deep specs that constrain this: `02-plugin-contract.md` (the contract these plugins prove), `04-orm-model-and-fields.md` (the `User` and `Session` models implement `Model`), `05-backends-and-system-check.md` (boot-time validator and cookie-flag checks).
- Sibling outlines: `web-layer.md` (the `Auth<U>` and `Session` extractors live here), `email.md` (password reset delivery), `forms.md` (login and password-change forms, validator sharing), `security-defaults.md` (password-validator defaults, secure-cookie flags, CSRF interaction with login POSTs).
