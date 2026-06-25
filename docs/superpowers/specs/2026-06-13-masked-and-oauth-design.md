# Masked field encryption + `umbral-oauth` — design

Status: approved 2026-06-13. Drives the implementation of two coupled features: a framework-level encrypt-at-rest field type (`Masked<T>`) and an OAuth/social-auth plugin (`umbral-oauth`) that stores provider tokens in `Masked` columns.

## Goals

- A reusable **field-encryption primitive** so any model can mark a column as encrypted-at-rest (`phone: Masked<String>`), for GDPR-style PII protection. Public-key encryption (encrypt with a public key, decrypt with a private key) so a write-only tier can store data it can't read, and so erasure can be done by crypto-shredding (dropping the private key).
- An **OAuth plugin** that does both **social login** ("Sign in with Google") and **account connection** ("Connect your GitHub"), layered on the existing `umbral-auth` `AuthUser` — extending it through a linked row, never replacing `username`.
- **Extensible** to new providers (and later to API-scoped connections like Google Drive) by implementing one trait.

## Non-goals (this pass)

- SAML / enterprise SSO, OIDC discovery beyond Google/GitHub, token-refresh background jobs, or actual Drive/GitHub API calls (the token store makes them possible later; we don't build them now).
- Generic `Masked<T>` over arbitrary `T` — ship `Masked<String>` first; widen later.
- Key rotation tooling beyond `maskkeygen` + a documented manual rotation procedure.

## Component A — `Masked<T>` (encrypt at rest)

### Crypto

X25519 **sealed boxes** (anonymous public-key encryption, libsodium `crypto_box_seal` semantics via the `crypto_box`/`dryoc` crate). Anyone with the public key can encrypt; only the private key can decrypt. ~48-byte ciphertext overhead per field. Chosen over `age` (smaller per-field ciphertext, no header) and over symmetric AES-GCM (the asymmetric requirement: write-only encryption + crypto-shredding).

### Type and API

`Masked<String>` — a wrapper that is **plaintext when freshly constructed** and **ciphertext when loaded from the DB**.

- Storage: base64 ciphertext in a `Text` column. The derive sees the field as `is_string_repr = true`, `widget = "masked"`.
- `Debug` / `Display` / `Serialize` → **redacted** (`"••••••"`). Masked data never leaks into logs, templates, or REST responses by default.
- `Masked::new(plaintext)` constructs a to-be-encrypted value.
- `.reveal() -> Result<String, MaskError>` decrypts. Requires the private key to be configured; returns a clear `MaskError::NoPrivateKey` otherwise.
- `.is_revealable()` reports whether the private key is present.

### ORM / derive integration

The encrypt-on-write / store-ciphertext-on-read boundary lives at the sqlx `Encode`/`Decode` impls for `Masked` (modeled on `FileField`):

- **Write** (`INSERT`/`UPDATE`): a freshly-constructed `Masked` encrypts its plaintext with the configured public key and binds the base64 ciphertext. A `Masked` loaded from the DB and re-saved binds its existing ciphertext unchanged (no re-encryption, no decrypt needed on the write path).
- **Read** (`FromRow`): stores the base64 ciphertext verbatim; decryption is deferred to `.reveal()`.

This is the single highest-risk integration point and gets a focused round-trip test first.

### Key management

- `UMBRAL_MASK_PUBLIC_KEY` (base64 X25519 public) and `UMBRAL_MASK_PRIVATE_KEY` (base64 X25519 secret, optional), resolved once into a `OnceLock` mask keyring — the same ambient-global pattern as the DB pool, with an explicit override available for tests.
- CLI command **`maskkeygen`**: generates a keypair and prints the two env lines plus a one-line GDPR note.
- Encryption needs only the public key; reveal needs the private key. A tier configured with only the public key can store but not read masked data.

### Security / GDPR notes (for the doc page)

- Crypto-shredding: deleting the private key renders all masked columns permanently unrecoverable — a fast bulk "right to be forgotten".
- Rotation: documented manual procedure (decrypt-with-old, re-encrypt-with-new via a management command — stubbed now, real command is a follow-up).

### Deliverables

`crates/umbral-core`: `orm/masked.rs` (type + crypto + keyring), facade re-export (`umbral::orm::Masked`, prelude). `crates/umbral-macros`: derive recognizes `Masked`. `crates/umbral-cli`: `maskkeygen`. Tests: encrypt→store→load→reveal round-trip; redaction in Debug/serde; public-key-only reveal error; migration column type is `Text`. Doc: `documentation/docs/v0.0.1/orm/masked.mdx`.

## Component B — `umbral-oauth`

Depends on the `auth` plugin. Crate `plugins/umbral-oauth`, `Plugin::name() == "oauth"`, `dependencies() == ["auth"]`.

### Model

`SocialAccount`:

- `user: ForeignKey<AuthUser>` (on_delete cascade) — the link. `username` is untouched; the social account is an extension row.
- `provider: String`, `provider_uid: String` — unique together `(provider, provider_uid)`.
- `provider_email: Option<String>`, `email_verified: bool`.
- `access_token: Masked<String>`, `refresh_token: Option<Masked<String>>`, `scopes: String`, `expires_at: Option<DateTime<Utc>>`.
- timestamps. Migration JSON committed.

### Provider abstraction

`trait OAuthProvider`: `authorize_url(state) -> Url`, `exchange_code(code) -> TokenSet`, `fetch_identity(&TokenSet) -> Identity { uid, email, email_verified }`. Implementations: `Google` (OIDC userinfo) and `GitHub` (user + emails endpoints). Built on the `oauth2` crate for the flow and `reqwest` for the identity fetch.

### Routes

- `GET /oauth/<provider>/login` → redirect to the provider with a CSRF `state` stored in the session.
- `GET /oauth/<provider>/callback` → validate `state`, exchange code, fetch identity, apply policy, establish session, redirect.
- `GET /oauth/<provider>/connect` (auth required) → same as login but binds to the current user.
- `POST /oauth/<provider>/disconnect` (auth required) → delete the `SocialAccount` row.

### Policy: create-or-link-by-verified-email

On callback with no existing `SocialAccount` for `(provider, uid)`:

1. If a request is authenticated (the `/connect` flow) → attach the provider to the logged-in user.
2. Else if the provider asserts a **verified** email matching an existing `AuthUser.email` → link to that user and log in.
3. Else → auto-create a new `AuthUser` (username derived from email/provider, uniqueness-guarded), link, and log in.

Email-based auto-linking happens **only** when `email_verified` is true (prevents account takeover via an unverified provider email). Login establishes the session via `umbral_sessions::login_user_id`.

### Settings

`OAuthSettings`: per-provider `{ client_id, client_secret, redirect_url, scopes }` from env (`UMBRAL_OAUTH_GOOGLE_CLIENT_ID`, …). `OAuthPlugin::default().google(...).github(...)` builder, or `from_env()`.

### Deliverables

Crate, model + migration, provider trait + Google/GitHub, routes, settings, behavioral tests (policy branches with a mocked provider identity; CSRF state rejection; disconnect). Doc: `documentation/docs/v0.0.1/auth/oauth.mdx`.

## Component C — website wiring

Add a "Connected accounts" section to `umbral_website`'s accounts page: a **Sign in with Google** entry point, and **Connect / Disconnect** controls for GitHub and Google for logged-in users. Register `OAuthPlugin` in `umbral_website/src/main.rs` with credentials from env. No fabricated secrets committed.

## Sequencing

Execute A → B → C. Each lands as its own gated commit(s) (`fmt`/`clippy`/`build`/`test`). A is independently useful (encrypt any PII field) and is the hard dependency for B's token columns.

## Risks / open questions

- **sqlx `Encode`/`Decode` for `Masked`** across SQLite + Postgres: the write path must distinguish freshly-constructed (encrypt) from loaded (pass-through) values. Resolved in the plan with an internal enum state; verified by the first round-trip test.
- **Derive recognition of `Masked`**: the macro currently keys field handling off known type names (`ImageField` precedent). `Masked<String>` is generic — confirm the macro can match a generic path, else fall back to a `#[umbral(masked)]` attribute.
- **`crypto_box` vs `dryoc` vs `age`**: pinned at implementation start by which exposes a clean sealed-box API on stable Rust with minimal deps.
