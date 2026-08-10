# Enterprise identity for umbral: SSO, MFA, and org lifecycle

| | |
|---|---|
| **Status** | Draft (covers planning/gaps5.md #9 tf#222, #10 tf#223, #11 tf#224) |
| **Date** | 2026-08-08 |
| **Touches** | new plugins `umbral-sso`, `umbral-mfa`, `umbral-org`; composes with `plugins/umbral-auth`, `plugins/umbral-oauth`, `plugins/umbral-sessions`, `plugins/umbral-permissions`, `plugins/umbral-tenants` |
| **Companions** | `docs/decisions/2026-06-28-auth-full-surface.md`, `docs/superpowers/specs/2026-06-13-masked-and-oauth-design.md`, `docs/decisions/2026-08-08-product-north-star.md` |

## Purpose and scope

This is a single cohesive design for the three enterprise-identity backlog items, treated as one program because they share a spine (the `umbral-auth` user model, the `umbral-sessions` session, and the `umbral-permissions` group graph) even though they ship as three separate plugins:

- **gaps5 #9 (tf#222): enterprise SSO and provider completeness.** Generic OIDC discovery plus SAML 2.0, while also closing the "Google/GitHub only" social-login gap with provider metadata, first-class social providers, and OAuth device-flow support. Generic SSO lands in a new `umbral-sso` plugin; social-provider breadth stays in `umbral-oauth`.
- **gaps5 #10 (tf#223): multi-factor auth.** TOTP, WebAuthn/passkeys, recovery codes, remembered devices, step-up auth, admin enforcement. Delivered as a new `umbral-mfa` plugin.
- **gaps5 #11 (tf#224): org identity lifecycle.** SCIM 2.0 provisioning, JIT provisioning, OIDC claims-to-groups mapping, domain verification, deprovisioning. Delivered as a new `umbral-org` plugin.

This is Stage 1/Stage 2 framework work per `docs/decisions/2026-08-08-product-north-star.md`: every capability is a plugin, there is no privileged core, and none of this presumes a managed control plane. The plugin contract stays the boundary. A REST-free, SSO-free app still compiles and runs with none of these crates present.

## What already exists (the baseline we build on)

Accurate to the code as of this date. New work reuses these types by their real names, never reinvents them.

From `plugins/umbral-auth/src/lib.rs`:

- **`AuthUser`** model (`id: i64`, `username`, `email`, `password_hash`, `is_active`, `is_staff`, `is_superuser`, `date_joined`, `last_login`, `email_verified_at`). Table `auth_user`.
- **`UserModel`** trait: the swap-in surface `AuthPlugin<U>` operates on. Required methods `id() -> <Self as Model>::PrimaryKey`, `username()`, `password_hash()`, `set_password_hash()`; defaulted `id_string()`, `login_columns()`, `is_active()`, `is_staff()`, `is_superuser()`. Every plugin below stays generic over `U: UserModel` wherever it touches the user, and FKs the concrete `AuthUser` only where the built-in token/challenge tables already do (guarded by the `TypeId::of::<U>() == TypeId::of::<AuthUser>()` check the auth plugin uses in `models()`).
- **`AuthPlugin<U = AuthUser>`** with its builders: `with_default_routes()` / `with_default_routes_at()`, `with_form_routes()`, `with_user_in_templates()`, `require_verified_email()`, `mailer()`, `with_db_session_var()`, `password_validators()`, and the throttle knobs. New plugins mirror this secure-by-default builder shape.
- **`AuthToken`** (opaque bearer token, hashed at rest via `digest_token`), **`AuthChallenge`** (the one-table verify/reset challenge store; the precedent for hashed, single-use, TTL-bound, attempt-capped secrets), and the helpers `create_user`, `authenticate`, `set_password`, `login`, `login_with_request`, `logout`, `hash_password`, `verify_password`.
- **`AuthError`** enum and the extractors `CurrentIdentity`, `OptionalIdentity`, `RequireAuth`, `RequireStaff`, `LoggedIn<U>`, plus `resolve_identity`.

From `plugins/umbral-oauth`:

- **`OAuthProvider`** trait: `key()`, `label()`, `trusts_verified_email()` (safe-by-default `false`; only providers that genuinely verify email ownership return `true`), `authorize_url(state, redirect_uri, code_challenge)`, `exchange_code(code, redirect_uri, code_verifier)`, `fetch_identity(tokens)`.
- **`TokenSet`** (`access_token`, `refresh_token`, `expires_in`, `scopes`) and **`Identity`** (`uid`, `email`, `email_verified`, `display_name`).
- **`SocialAccount`** model: FK `user: ForeignKey<AuthUser>`, `provider`, `provider_uid`, `provider_email`, `email_verified`, `access_token: Masked<String>`, `refresh_token: Option<Masked<String>>`, `scopes`, `expires_at`. Unique on `(provider, provider_uid)`.
- **`OAuthPlugin`** with `new`, `provider`, `provider_opt`, `from_settings`, `login_redirect`, `allow_return`; `dependencies()` returns `["auth", "sessions"]`; the create-or-link policy plus PKCE and `state` CSRF defense already live in `routes.rs` / `pkce.rs` / `policy.rs`.
- **`Masked<String>`** (X25519 sealed-box encrypt-at-rest field), used for every stored provider secret.

From `plugins/umbral-permissions`: **`Group`** (`permissions_group`), **`UserGroup`** (`permissions_usergroup`, `user_id` is `String` so it is PK-agnostic), **`Permission`**, **`UserPermission`**, **`ObjectPermission`**, and the membership helpers `add_user_to_group(user_id, &Group)`, `remove_user_from_group`, `set_user_groups(user_id, &[i64])`, `groups_for_user`, `group_ids_for_user`. The group graph keys on `id_string()`, which is exactly the seam group-sync needs.

From `plugins/umbral-tenants`: **`Tenant`** (`schema_name`, `name`, `domain`, `is_active`), **`TenantsPlugin`** (with `create_tenant`, `TenantStrategy`, the `TenantMembership` trait). An org (below) maps optionally onto a tenant for the multi-tenant deployments, but org identity does not require tenancy.

### What was explicitly deferred (and is now in scope)

- `docs/superpowers/specs/2026-06-13-masked-and-oauth-design.md` non-goals: "SAML / enterprise SSO, OIDC discovery beyond Google/GitHub, token-refresh background jobs." gaps5 #9 picks up SAML, generic OIDC discovery, social-provider breadth beyond Google/GitHub, and OAuth device-flow support. Token-refresh jobs do not block login, but they are no longer hand-waved: a provider that advertises API-access/connection use must either implement refresh or explicitly mark itself login-only.
- `docs/decisions/2026-06-28-auth-full-surface.md` out-of-scope list: "TOTP / 2FA", "Magic-link / passwordless login". gaps5 #10 picks up TOTP and passkeys. (Note: authenticated `change_password` was on that deferred list but has since shipped in the auth `challenge` module; step-up auth in #10 reuses it.)

## Composition model: how each plugin plugs into auth without a privileged core

Every plugin here is structurally identical to `umbral-oauth`: it depends on the `umbral` facade, declares `dependencies()` naming `"auth"` and `"sessions"` (and `"permissions"` where it syncs groups), contributes its own models (which become migrations), mounts its own routes, and reads the ambient DB pool through the ORM. None of them is special-cased inside `umbral-core`.

The one shared idea: **an external identity is an extension row keyed to the user, never a replacement for the user.** `umbral-oauth` already established this with `SocialAccount`. `umbral-sso` follows the same shape with `SsoIdentity`. Sign-in through any of these resolves (or, subject to policy, provisions) an `AuthUser` and then establishes a session exactly the way `umbral_sessions::login` / `umbral_auth::login_with_request` already does. The MFA plugin sits between "credentials verified" and "session established", gating the session mint. This keeps the login terminal (`umbral-sessions` session + optional `AuthToken` bearer) as the single choke point every path funnels through.

Session establishment stays owned by `umbral-sessions`, so session fixation is handled the way it already is: a new session id is issued on privilege change. Every new sign-in path below MUST route through the existing login helper rather than writing a session row directly, so it inherits that rotation for free.

---

## Section A0: `umbral-oauth` provider catalog and device flow (part of gaps5 #9, tf#222)

The immediate code gap behind #9 is not only enterprise SSO. The shipped OAuth plugin has a good provider abstraction, but the built-ins are Google and GitHub only. For adoption, `umbral-oauth` needs a visible provider catalog, enough built-in adapters for common apps, and a non-browser/CLI device flow. These stay in `umbral-oauth`; they do not belong in `umbral-sso` unless the provider is being used as an enterprise IdP with domain routing and group claims.

### Provider tiers

Keep `OAuthProvider` as the low-level trait. Add a higher-level `ProviderManifest` for documentation, admin UI, boot checks, and generated settings docs:

```rust
pub struct ProviderManifest {
    pub key: &'static str,
    pub label: &'static str,
    pub protocol: ProviderProtocol,          // OAuth2 | Oidc
    pub default_scopes: &'static [&'static str],
    pub supports_pkce: bool,
    pub supports_refresh_token: bool,
    pub supports_device_authorization: bool,
    pub email_trust: EmailTrust,             // Verified | Conditional | Untrusted | NotProvided
    pub login_use: ProviderUse,              // LoginAndConnect | ConnectOnly | LoginOnly
    pub docs_url: &'static str,
}
```

The first-party adapter list should be explicit:

| Provider | Why it matters | Notes |
|---|---|---|
| Google | already shipped; consumer and Workspace login | OIDC userinfo today; generic OIDC ID-token verification can replace the hand-coded identity trust path later. |
| GitHub | already shipped; developer login/connect | No refresh token for OAuth apps; email trust comes from `/user/emails`. |
| Apple | required for serious consumer/mobile apps | OIDC; email may be private relay and often appears only on first consent; verify `iss`, `aud`, `nonce`, and JWK signature. |
| Microsoft | consumer Microsoft accounts plus Entra ID | Prefer OIDC discovery; tenant mode decides whether it is social login, enterprise SSO, or both. |
| Facebook | broad consumer login | Graph API identity; email may be absent depending on permissions, so auto-link only on a verified email assertion. |
| X / Twitter | social login and account connect | OAuth 2.0; email is often unavailable, so default `email_trust = NotProvided` and do not auto-link by email. |
| LinkedIn | business/professional login | OIDC when available; good candidate for verified-email linking only after adapter tests prove the claim semantics. |
| GitLab | developer/org login | OAuth/OIDC depending on deployment; useful for self-hosted GitLab instances through custom endpoints. |
| Bitbucket | developer login/connect | OAuth 2.0; mostly account connection and import workflows. |
| Discord | community products | OAuth 2.0; email can be verified but scopes are explicit, so manifest must state the trust rule. |
| Slack | workspace install/connect | Primarily connect/workspace authorization, not default user login; use `ProviderUse::ConnectOnly` unless sign-in is explicitly enabled. |
| Mastodon/custom OAuth2 | federation/self-hosted apps | Generic OAuth2 adapter with custom authorize/token/userinfo endpoints, marked untrusted for email by default. |

The default `OAuthPlugin::from_settings` keeps Google/GitHub for backwards compatibility, then grows a provider registry convention: `UMBRAL_OAUTH_<KEY>_CLIENT_ID`, `UMBRAL_OAUTH_<KEY>_CLIENT_SECRET`, optional `UMBRAL_OAUTH_<KEY>_SCOPES`, and, for custom OAuth2/OIDC, endpoint/discovery URLs. A half-configured provider remains a boot warning and is skipped, matching today's safe posture.

### Device Authorization Grant

Add OAuth 2.0 Device Authorization Grant support (RFC 8628) for CLI tools, TVs, terminals, and native apps that cannot safely receive a browser callback. This is not a replacement for browser login; it is a separate public-client flow that still ends at the same `AuthUser` + `umbral-sessions`/`AuthToken` terminal.

Routes under `/oauth/device`:

| Method | Path | Purpose |
|---|---|---|
| POST | `/oauth/device/code` | Client asks for a `device_code`, short `user_code`, verification URL, TTL, and polling interval. |
| GET/POST | `/oauth/device/verify` | Human enters the `user_code` in a normal browser session, authenticates, satisfies MFA if required, and approves the device. |
| POST | `/oauth/device/token` | Device polls with `device_code`; before approval returns `authorization_pending` / `slow_down`; after approval returns a bearer token. |

Back it with an `OAuthDeviceGrant` model: hashed `device_code`, short `user_code_hash`, provider/client label, requested scopes, `expires_at`, `approved_user_id`, `approved_at`, `denied_at`, `last_polled_at`, and `poll_interval_secs`. Codes are single-use and TTL-bound; polling is throttled and returns the RFC errors. Approval uses the normal login stack, so `umbral-mfa` gates it when policy requires MFA, and `umbral-org` domain/group policy can deny approval when the provider/user is not allowed.

Security posture:

- Device clients are public clients; no client secret is accepted or required.
- `user_code` is short only because the browser session authenticates the human; the high-entropy `device_code` is stored hashed and never displayed.
- A device token is issued only after the browser session approves. Approval should show provider/app label, requested scopes, and expiry.
- Tokens issued by device flow are named bearer tokens, so `DeviceSession` inventory (#13) can list and revoke them.
- Device flow is disabled by default and enabled per provider/client. Browser/mobile apps keep Authorization Code + PKCE.

## Section A: `umbral-sso` (gaps5 #9, tf#222): generic OIDC and SAML 2.0

### Honesty up front

OIDC discovery is a bounded, well-specified addition that mostly reuses the OAuth machinery already present. SAML 2.0 is large: XML canonicalization, XML digital signatures, assertion condition validation, and two initiation modes (SP-initiated and IdP-initiated) are each a meaningful surface with real security footguns. We phase accordingly (see "Phasing" below): OIDC discovery first, SAML second, and SAML behind its own cargo feature so an OIDC-only deployment never compiles an XML-DSig stack.

### Plugin shape and composition

`umbral-sso` is a new crate under `plugins/`. It depends on the `umbral` facade and on `umbral-auth` (for `AuthUser`, `UserModel`, and the login helper) and `umbral-sessions`. Its `Plugin::dependencies()` returns `["auth", "sessions"]`, and `["permissions"]` is added when the claims-to-groups mapping is enabled (that mapping itself lives in `umbral-org`, section C; `umbral-sso` just exposes the parsed claims/attributes to it).

It reuses the OAuth abstractions where they fit. An OIDC provider IS an `OAuthProvider` with two additions: it discovers its endpoints instead of hardcoding them, and it verifies a signed ID token instead of trusting a userinfo fetch. So the OIDC side introduces a small extension rather than a parallel world.

```rust
// The wiring a consumer writes.
App::builder()
    .plugin(AuthPlugin::new().with_form_routes())
    .plugin(SessionsPlugin::default())
    .plugin(
        SsoPlugin::new("https://app.example.com")
            // Generic OIDC by discovery document.
            .oidc(OidcProvider::discover("okta",
                "https://acme.okta.com/.well-known/openid-configuration")
                .client_id_from_env("SSO_OKTA_CLIENT_ID")
                .client_secret_from_env("SSO_OKTA_CLIENT_SECRET"))
            // SAML 2.0 (feature = "saml").
            .saml(SamlProvider::from_metadata_url("azuread",
                "https://login.microsoftonline.com/<tenant>/federationmetadata/..."))
            // Enterprise routing: which provider a work email lands on.
            .domain("acme.com", "okta")
            .domain("contoso.com", "azuread"),
    )
```

`SsoPlugin::new(base)` mirrors `OAuthPlugin::new(base)`: the base is the app's public origin, and per-provider callback / ACS URLs are built from it. `.oidc(...)` and `.saml(...)` register providers keyed by a stable `key` (the string that appears in routes and stored rows), exactly as `OAuthPlugin::provider` does. `.domain(email_domain, provider_key)` records the email-domain-to-provider mapping.

### Models

Four models, all owned by the plugin (`#[umbral(plugin = "sso")]`), all migrated through the normal loop.

**`SsoProvider`** stores provider metadata (the "provider metadata storage" the gap asks for) so an operator can add or edit a provider through the admin without a redeploy, and so discovery/metadata documents are cached rather than refetched on every login:

```rust
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbral(plugin = "sso", display = "SSO providers", icon = "shield",
         unique_together = [["key"]])]
pub struct SsoProvider {
    pub id: i64,
    #[umbral(unique, max_length = 64)] pub key: String,      // "okta", "azuread"
    #[umbral(max_length = 120)] pub label: String,
    #[umbral(max_length = 8)] pub protocol: String,          // "oidc" | "saml"
    pub is_active: bool,

    // OIDC: discovered + cached from the .well-known document.
    #[umbral(max_length = 512)] pub issuer: Option<String>,
    #[umbral(max_length = 512)] pub authorization_endpoint: Option<String>,
    #[umbral(max_length = 512)] pub token_endpoint: Option<String>,
    #[umbral(max_length = 512)] pub jwks_uri: Option<String>,
    #[umbral(max_length = 255)] pub client_id: Option<String>,
    pub client_secret: Option<Masked<String>>,               // encrypt at rest
    pub jwks_cache: Option<String>,                          // cached JWKS JSON
    pub metadata_fetched_at: Option<DateTime<Utc>>,

    // SAML: SP config plus the IdP half parsed from metadata.
    #[umbral(max_length = 512)] pub saml_entity_id: Option<String>,        // IdP EntityID
    #[umbral(max_length = 512)] pub saml_sso_url: Option<String>,          // IdP SingleSignOnService
    pub saml_idp_certs: Option<String>,                     // PEM chain(s) for signature verify
    #[umbral(max_length = 40)] pub saml_name_id_format: Option<String>,

    #[umbral(auto_now_add)] pub created_at: DateTime<Utc>,
    #[umbral(auto_now)]     pub updated_at: DateTime<Utc>,
}
```

**`SsoIdentity`** is the `SocialAccount` analogue: one external identity linked to a user, at most one per (provider, subject).

```rust
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbral(plugin = "sso", display = "SSO identities", icon = "id-card",
         unique_together = [["provider", "subject"]])]
pub struct SsoIdentity {
    pub id: i64,
    #[umbral(on_delete = "cascade")] pub user: ForeignKey<AuthUser>,
    #[umbral(index, max_length = 64)] pub provider: String,   // SsoProvider.key
    #[umbral(max_length = 255)] pub subject: String,          // OIDC `sub` / SAML NameID
    #[umbral(max_length = 320)] pub email: Option<String>,
    #[umbral(default = "false")] pub email_verified: bool,
    pub raw_claims: Option<String>,                          // last claims/attrs (JSON), for group sync
    pub last_login: Option<DateTime<Utc>>,
    #[umbral(auto_now_add)] pub created_at: DateTime<Utc>,
    #[umbral(auto_now)]     pub updated_at: DateTime<Utc>,
}
```

**`SsoDomain`** persists the email-domain-to-provider mapping so it too is admin-editable (the builder `.domain(...)` seeds defaults; DB rows win at runtime):

```rust
pub struct SsoDomain {
    pub id: i64,
    #[umbral(unique, max_length = 255)] pub domain: String,   // "acme.com"
    #[umbral(index, max_length = 64)]   pub provider: String,  // SsoProvider.key
    pub is_active: bool,
    // gaps5 #11 domain verification is enforced here: a mapping is honored
    // for auto-provisioning only when the domain is verified (see section C).
    pub verified_at: Option<DateTime<Utc>>,
}
```

**`SsoAuthState`** is the short-lived per-flow state row (nonce for OIDC, RelayState/InResponseTo for SAML), so callbacks can validate against server-side state rather than trusting the returned document. It is single-use and TTL-bound, following the `AuthChallenge` precedent (hashed where it is a secret, pruned on use/expiry). For OIDC this can alternatively ride the session the way `umbral-oauth` already stashes its `state`; a DB row is used for SAML because IdP-initiated flows have no prior session.

### Routes and flows

Mounted under `/sso` (mirroring `/oauth`):

| Method | Path | Purpose |
|---|---|---|
| GET | `/sso/start?email=<addr>` | Home-realm discovery: look up the domain, redirect to the right provider's login. |
| GET | `/sso/{key}/login` | Begin the provider flow (OIDC authorize redirect, or SP-initiated SAML AuthnRequest). |
| GET | `/sso/{key}/callback` | OIDC redirect target: exchange code, verify ID token, resolve/provision, login. |
| POST | `/sso/{key}/acs` | SAML Assertion Consumer Service: consume the SAMLResponse, verify signature/conditions, resolve/provision, login. |
| GET | `/sso/{key}/metadata` | SP metadata XML (for the operator to hand the IdP). |
| GET | `/sso/providers` | Discovery surface listing active providers (parallels `/oauth/providers`). |

**OIDC flow.**
1. `/sso/{key}/login` loads (or discovers-and-caches) the provider's `.well-known/openid-configuration`, then builds the authorize URL. It reuses the existing PKCE module (`code_challenge`/`code_verifier` with `S256`) and a fresh `nonce`, storing both server-side (session or `SsoAuthState`).
2. The IdP redirects back to `/sso/{key}/callback` with `code` + `state`. We validate `state` (CSRF), exchange the `code` at the discovered `token_endpoint`, and receive an ID token (a JWS).
3. **ID-token verification** (the load-bearing step): fetch JWKS from the discovered `jwks_uri` (cached in `SsoProvider.jwks_cache`, keyed by `kid`, refetched on unknown `kid`), verify the JWS signature, then assert `iss` equals the configured issuer, `aud` contains our `client_id`, `exp` is in the future within a small clock-skew allowance, and `nonce` equals the stored nonce. Only then is `sub` trusted as the identity.
4. Resolve `SsoIdentity` by `(provider, sub)`. Create-or-link uses the same policy `umbral-oauth` already ships (link to an existing `AuthUser` by verified email only when the provider is trusted for that domain; otherwise provision a fresh user subject to org policy in section C), then call the auth login helper.

**SAML SP-initiated flow.** `/sso/{key}/login` builds a SAML `AuthnRequest`, records `InResponseTo` + `RelayState` in `SsoAuthState`, and redirects (HTTP-Redirect binding) to the IdP `SingleSignOnService`. The IdP posts a `SAMLResponse` to `/sso/{key}/acs` (HTTP-POST binding). We verify the assertion (below), match `InResponseTo` to our stored state, then resolve/provision and login.

**SAML IdP-initiated flow.** The IdP posts an unsolicited `SAMLResponse` to `/sso/{key}/acs` with no prior `AuthnRequest`. There is no `InResponseTo` to match, which is exactly why IdP-initiated is riskier (no server-side proof the flow was intended). We accept it only when the provider row opts in (`allow_idp_initiated`, default off), and we lean entirely on assertion signature + condition validation plus a replay cache.

**SAML assertion validation** (every arm rejects with the same generic error, no enumeration):
- **Signature.** Verify the XML-DSig over the assertion (or the response) against `SsoProvider.saml_idp_certs`. Require a signature; never accept an unsigned assertion. Canonicalize before verifying. Reject if the signature covers a different element than the one we consume (XML Signature Wrapping defense: validate that the signed element is the assertion we actually read).
- **Conditions.** Enforce `NotBefore` / `NotOnOrAfter` with a bounded clock-skew window, `Audience` equals our SP EntityID, and `Recipient` equals our ACS URL.
- **Replay.** Cache the `Assertion@ID` (and enforce single use within the validity window) so a captured assertion cannot be replayed. This is the SAML analogue of the OIDC `nonce`.
- **Subject.** Only after all of the above is `NameID` trusted as the identity `subject`.

### Security considerations (Section A)

- **Replay:** OIDC `nonce` bound server-side and checked on callback; SAML `Assertion@ID` replay cache + `InResponseTo` match for SP-initiated.
- **Signature:** JWKS signature verification for OIDC (reject `alg: none`, pin to the discovered keys, refetch on unknown `kid`); mandatory XML-DSig verification for SAML with Signature-Wrapping protection.
- **Clock skew:** a single configurable allowance (default 60 seconds) applied to OIDC `exp`/`iat` and SAML `NotBefore`/`NotOnOrAfter`.
- **Downgrade:** OIDC pins `S256` PKCE and rejects a token whose `aud`/`iss` do not match; SAML requires signatures and refuses the unsigned path; the discovery document is fetched over TLS and the issuer in it must match the configured issuer.
- **Session fixation:** login goes through `umbral-sessions`, which rotates the session id on authentication.
- **Open redirect:** `/sso/start` and any post-login redirect reuse the same same-site relative-path allowlist the auth form routes already enforce.

### Config / settings (Section A)

Per-provider via the builder or `SsoProvider` rows. Env-driven convention mirrors `OAuthPlugin::from_settings`: `UMBRAL_SSO_<KEY>_CLIENT_ID`, `UMBRAL_SSO_<KEY>_CLIENT_SECRET`, `UMBRAL_SSO_<KEY>_DISCOVERY_URL` (OIDC) or `_METADATA_URL` (SAML). Global knobs: `clock_skew_secs`, `allow_idp_initiated`, `jwks_cache_ttl`. A `SsoProvider::from_settings(&Settings)` constructor exists so a full env-only deployment works, and a provider with a half-set credential pair is skipped with a warning (same posture as OAuth).

### Admin surface (Section A)

`SsoProvider`, `SsoIdentity`, and `SsoDomain` register in the admin as normal models (the `#[umbral(plugin = "sso", display = ..., icon = ...)]` attributes drive the nav). `client_secret` and any cert material render through `Masked` (redacted). A small admin custom view (using the `AdminPlugin::view(AdminView)` widget surface) offers "Refresh discovery/metadata now" and "Test login" actions per provider.

---

## Section B: `umbral-mfa` (gaps5 #10, tf#223): TOTP, passkeys, recovery, step-up

### Honesty up front

TOTP plus recovery codes is small and high-value; it ships first. WebAuthn/passkeys is large: attestation, the registration and authentication ceremonies, credential and signature-counter storage, and browser-side JS all carry real complexity and are easy to get subtly wrong. Passkeys ship second, behind a cargo feature, on top of a well-known verified crate (do not reimplement the primitive; stand on `webauthn-rs` or equivalent).

### Plugin shape and composition

`umbral-mfa` is a new crate. `Plugin::dependencies()` returns `["auth", "sessions"]`. It is generic over `U: UserModel` for reads but FKs `AuthUser` for its enrollment tables (same `TypeId` guard pattern). Its central job is to insert a verification step between "credentials verified" and "session established", so it exposes a gate the login paths call rather than owning a parallel login.

The gate is a function plus a middleware, both of which any sign-in path (password login in `umbral-auth`, `umbral-oauth`, `umbral-sso`) can call:

```rust
// Called by a login handler after credentials verify but before the
// session is minted. Returns whether a second factor is still required.
pub async fn mfa_status(user_id: &str) -> Result<MfaGate, MfaError>;

pub enum MfaGate {
    NotEnrolled,             // no factors -> proceed (unless policy forces enrollment)
    Satisfied,               // remembered-device cookie already covers this login
    ChallengeRequired(MfaChallengeToken),  // must complete a factor next
}
```

A login that gets `ChallengeRequired` establishes only a partial, un-elevated session (or a short-lived pending token) and redirects to `/mfa/challenge`; the full session is minted only after a factor verifies. This keeps `umbral-sessions` as the single terminal and avoids a second session concept.

```rust
App::builder()
    .plugin(AuthPlugin::new().with_form_routes())
    .plugin(SessionsPlugin::default())
    .plugin(
        MfaPlugin::new()
            .totp()                       // ship first
            .recovery_codes(10)
            .remember_device_for(Duration::from_secs(30 * 24 * 3600))
            .passkeys("app.example.com", "https://app.example.com")  // feature = "webauthn"
            .enforce(MfaPolicy::RequiredForStaff),  // admin enforcement policy
    )
```

### Models

**`MfaFactor`** (one row per enrolled factor; a user may hold several):

```rust
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbral(plugin = "mfa", display = "MFA factors", icon = "key-round")]
pub struct MfaFactor {
    pub id: i64,
    #[umbral(on_delete = "cascade")] pub user: ForeignKey<AuthUser>,
    #[umbral(index, max_length = 16)] pub kind: String,      // "totp" | "webauthn"
    #[umbral(max_length = 120)] pub label: Option<String>,   // "iPhone", "YubiKey 5"

    // TOTP: the shared secret, encrypted at rest.
    pub totp_secret: Option<Masked<String>>,

    // WebAuthn: credential id + COSE public key + signature counter.
    #[umbral(max_length = 512)] pub credential_id: Option<String>,  // base64url
    pub public_key: Option<String>,                                 // COSE key
    pub sign_count: Option<i64>,
    #[umbral(max_length = 40)] pub aaguid: Option<String>,

    pub confirmed_at: Option<DateTime<Utc>>,   // enrollment completes only after one verify
    pub last_used_at: Option<DateTime<Utc>>,
    #[umbral(auto_now_add)] pub created_at: DateTime<Utc>,
}
```

**`RecoveryCode`** (one row per code, hashed at rest exactly like `AuthToken`, single-use):

```rust
pub struct RecoveryCode {
    pub id: i64,
    #[umbral(on_delete = "cascade")] pub user: ForeignKey<AuthUser>,
    #[umbral(index, max_length = 64)] pub code_hash: String,  // digest_token-style
    pub used_at: Option<DateTime<Utc>>,
    #[umbral(auto_now_add)] pub created_at: DateTime<Utc>,
}
```

**`RememberedDevice`** (a hashed opaque token cookie that lets a known browser skip the factor for a bounded window):

```rust
pub struct RememberedDevice {
    pub id: i64,
    #[umbral(on_delete = "cascade")] pub user: ForeignKey<AuthUser>,
    #[umbral(index, max_length = 64)] pub token_hash: String,
    #[umbral(max_length = 255)] pub user_agent: Option<String>,
    pub expires_at: DateTime<Utc>,
    #[umbral(auto_now_add)] pub created_at: DateTime<Utc>,
}
```

Challenge state (the in-flight TOTP/WebAuthn ceremony, and the pending un-elevated login) reuses the `AuthChallenge` pattern: hashed, single-use, TTL-bound, attempt-capped. A `WebAuthnChallenge` sub-row holds the ceremony `challenge` bytes for the duration of a registration or authentication.

### Routes and flows

Mounted under `/mfa`. JSON and form-action variants both provided, matching the auth plugin's two-surface approach.

| Method | Path | Purpose |
|---|---|---|
| POST | `/mfa/totp/enroll` | Create an unconfirmed TOTP `MfaFactor`, return the secret + otpauth URI (for a QR the app renders). |
| POST | `/mfa/totp/confirm` | Verify a code, set `confirmed_at`. |
| POST | `/mfa/challenge` | Submit a TOTP code (or recovery code) to satisfy a pending login. |
| GET/POST | `/mfa/webauthn/register/{begin,finish}` | Passkey registration ceremony (feature). |
| GET/POST | `/mfa/webauthn/auth/{begin,finish}` | Passkey authentication ceremony (feature). |
| POST | `/mfa/recovery/generate` | (Re)generate recovery codes, show once, store hashed. |
| POST | `/mfa/recovery/consume` | Satisfy a challenge with a recovery code. |
| POST | `/mfa/step-up` | Re-verify a factor for a sensitive action within an already-authenticated session. |
| DELETE | `/mfa/factor/{id}` | Remove a factor (step-up protected). |

**Enrollment (TOTP):** `enroll` generates a random base32 secret, stores it `Masked`, returns the `otpauth://` URI; `confirm` verifies one code to prove the clock is aligned before the factor counts. A factor is never active until `confirmed_at` is set.

**Login gate:** after `authenticate` succeeds, the login handler calls `mfa_status`. `ChallengeRequired` yields a pending token; the user posts `/mfa/challenge`; on success the full session mints. Recovery codes are an alternate satisfier of the same challenge.

**Step-up auth:** a handler guarding a sensitive action (change email, rotate API keys, delete account) calls a `require_recent_mfa(session, max_age)` helper. If the session has no recent factor verification, it returns a challenge that `/mfa/step-up` satisfies, stamping a `mfa_verified_at` marker on the session. This reuses the same factor-verification code as login.

**Remembered devices:** on a satisfied challenge the user may opt to remember the browser; we set a hashed cookie backed by `RememberedDevice`. `mfa_status` returns `Satisfied` when a live remembered-device token matches, skipping the factor for that window. The cookie is scoped, `HttpOnly`, `Secure`, `SameSite=Lax`, and single-purpose.

### Admin enforcement policies

`MfaPolicy` drives whether MFA is optional, encouraged, or mandatory:

```rust
pub enum MfaPolicy {
    Optional,             // users may enroll; login never blocks
    RequiredForStaff,     // is_staff() users must enroll before full access
    RequiredForAll,       // everyone must enroll
    RequiredForGroups(Vec<String>),  // gated on umbral-permissions group membership
}
```

When a policy requires MFA and a user has no confirmed factor, `mfa_status` returns `ChallengeRequired` in an "enrollment forced" mode: the user gets an un-elevated session that can reach only the enrollment routes until a factor is confirmed. `RequiredForStaff` reads `UserModel::is_staff()`; `RequiredForGroups` reads `umbral-permissions` group membership by `id_string()`. The admin surface lists enrolled factors per user and lets a staff admin reset a locked-out user's factors (itself a step-up-protected action).

### Security considerations (Section B)

- **Replay:** TOTP codes are single-use within their 30-second step (cache the last accepted step per factor); recovery codes are single-use (`used_at`); WebAuthn's signature counter is checked to be monotonic (a decrease signals a cloned authenticator).
- **Signature / clock skew:** TOTP accepts a small `±1` step window (configurable) for device clock drift; WebAuthn signatures are verified by the underlying crate against the stored COSE key.
- **Downgrade:** recovery codes and remembered-device tokens never bypass a *policy-required* enrollment; step-up cannot be satisfied by a stale remembered-device cookie. Secrets (`totp_secret`, recovery-code hashes, remembered-device hashes) never leave the server in cleartext after enrollment.
- **Session fixation / brute force:** the challenge is attempt-capped (reusing the `AuthChallenge` counter pattern) and rate-limited through the existing `Throttle` infra; the session is minted only post-verification via `umbral-sessions` (rotating the id).

### Config / settings (Section B)

Builder: `.totp()`, `.recovery_codes(n)`, `.remember_device_for(Duration)`, `.passkeys(rp_id, rp_origin)`, `.enforce(MfaPolicy)`, `.totp_skew(steps)`. Env fallbacks under `UMBRAL_MFA_*`. Passkeys require the WebAuthn Relying Party id and origin, which must match the deployment host, validated at boot via a `Plugin` system check.

---

## Section C: `umbral-org` (gaps5 #11, tf#224): SCIM, JIT, group mapping, domain verification, deprovisioning

### Plugin shape and composition

`umbral-org` is a new crate that owns the organization lifecycle. `Plugin::dependencies()` returns `["auth", "sessions", "permissions"]` (it writes the `umbral-permissions` group graph) and optionally composes with `umbral-tenants` (an `Org` may map to a `Tenant`) and `umbral-sso` (it consumes the claims/attributes `umbral-sso` parses). It is the piece that turns "a person authenticated" into "a provisioned, grouped, deprovisionable org member".

It reuses, never reinvents, the permission membership helpers: `add_user_to_group`, `remove_user_from_group`, `set_user_groups(user_id, &[i64])`, keyed on `UserModel::id_string()`. Group sync is literally a diff-and-apply over those helpers.

### Models

**`Org`** (the organization; optionally 1:1 with a `Tenant`):

```rust
pub struct Org {
    pub id: i64,
    #[umbral(unique, max_length = 120)] pub slug: String,
    #[umbral(max_length = 200)] pub name: String,
    pub tenant: Option<ForeignKey<Tenant>>,     // optional link to umbral-tenants
    #[umbral(max_length = 64)] pub sso_provider: Option<String>,  // default SsoProvider.key
    pub is_active: bool,
    #[umbral(auto_now_add)] pub created_at: DateTime<Utc>,
}
```

**`OrgDomain`** (a claimed email domain plus its verification state; the SSO `SsoDomain` mapping honors verification recorded here):

```rust
pub struct OrgDomain {
    pub id: i64,
    #[umbral(on_delete = "cascade")] pub org: ForeignKey<Org>,
    #[umbral(unique, max_length = 255)] pub domain: String,
    #[umbral(max_length = 120)] pub verification_token: String,  // DNS TXT value to publish
    pub verified_at: Option<DateTime<Utc>>,
}
```

**`OrgMembership`** (who belongs to an org and in what role; deprovisioning flips `status`):

```rust
pub struct OrgMembership {
    pub id: i64,
    #[umbral(on_delete = "cascade")] pub org: ForeignKey<Org>,
    #[umbral(on_delete = "cascade")] pub user: ForeignKey<AuthUser>,
    #[umbral(max_length = 16)] pub role: String,             // "member" | "admin" | "owner"
    #[umbral(index, max_length = 16)] pub status: String,    // "active" | "suspended" | "deprovisioned"
    #[umbral(max_length = 40)] pub source: String,           // "scim" | "jit" | "manual"
    pub scim_external_id: Option<String>,                    // the IdP's stable user id
    pub deprovisioned_at: Option<DateTime<Utc>>,
}
```

**`ScimToken`** (a hashed-at-rest bearer credential the IdP presents to the SCIM endpoints; the same `digest_token` precedent as `AuthToken`):

```rust
pub struct ScimToken {
    pub id: i64,
    #[umbral(on_delete = "cascade")] pub org: ForeignKey<Org>,
    #[umbral(index, max_length = 64)] pub token_hash: String,
    #[umbral(max_length = 120)] pub label: String,
    pub revoked_at: Option<DateTime<Utc>>,
    #[umbral(auto_now_add)] pub created_at: DateTime<Utc>,
}
```

**`GroupMapping`** (declarative claim/attribute-to-`Group` rules, admin-editable):

```rust
pub struct GroupMapping {
    pub id: i64,
    #[umbral(on_delete = "cascade")] pub org: ForeignKey<Org>,
    #[umbral(max_length = 120)] pub claim: String,           // e.g. "groups" | "roles"
    #[umbral(max_length = 200)] pub claim_value: String,     // e.g. "Engineering"
    pub group: ForeignKey<Group>,                            // target umbral-permissions Group
}
```

### Flows

**JIT provisioning.** When `umbral-sso` (or `umbral-oauth`) resolves an identity with no matching `AuthUser`, and the email domain is a *verified* `OrgDomain`, `umbral-org` creates the `AuthUser` (via `create_user` with an unusable password, since the credential lives at the IdP), creates an `active` `OrgMembership` with `source = "jit"`, and applies group mappings. Unverified domains never auto-provision (anti-takeover); they either reject or create an orphan pending admin approval, per policy.

**SCIM 2.0 provisioning.** The plugin mounts the standard SCIM endpoints under `/scim/v2`, authenticated by a `ScimToken` bearer scoped to one `Org`:

| Method | Path | Purpose |
|---|---|---|
| POST/GET/PUT/PATCH/DELETE | `/scim/v2/Users` (+`/{id}`) | Create, read, replace, patch, deprovision users. |
| POST/GET/PUT/PATCH/DELETE | `/scim/v2/Groups` (+`/{id}`) | Manage groups (mapped onto `umbral-permissions` `Group`). |
| GET | `/scim/v2/ServiceProviderConfig`, `/ResourceTypes`, `/Schemas` | SCIM discovery documents. |

SCIM `User` maps to `AuthUser` + `OrgMembership` (`scim_external_id` is the IdP's stable id; `active: false` in a PATCH sets `status = "deprovisioned"` and deactivates the `AuthUser` via `is_active = false`). SCIM `Group` membership maps to `UserGroup` through the permission helpers. All writes go through the ORM, never raw SQL, per the plugin rule.

**OIDC claims-to-groups mapping.** On each SSO login, `umbral-sso` hands the parsed claims (stored in `SsoIdentity.raw_claims`) to `umbral-org`, which evaluates `GroupMapping` rows for the org and computes the target group id set, then calls `set_user_groups(user.id_string(), &target_ids)` to make membership match the IdP exactly (adds and removes). This is the "IdP is the source of truth for groups" posture; a per-org flag can switch it to additive-only when the app also manages local groups.

**Domain verification.** `POST /org/{slug}/domains` issues a `verification_token`; the operator publishes a DNS TXT record (`umbral-verify=<token>`); `POST /org/{slug}/domains/{id}/verify` performs the DNS lookup and stamps `verified_at`. Only verified domains gate JIT provisioning and email-based auto-linking.

**Deprovisioning.** A SCIM `DELETE`/`active:false`, or an admin action, sets `OrgMembership.status = "deprovisioned"`, sets `AuthUser.is_active = false` (so `authenticate` rejects them, matching the existing inactive-user gate), revokes their sessions and `AuthToken`s (reusing the auth reset-sweep), and removes IdP-sourced group memberships. The row is retained (not hard-deleted) for audit.

### Security considerations (Section C)

- **SCIM auth:** `ScimToken` is a hashed-at-rest bearer scoped to one org; a leaked DB never yields a live token, and revocation is immediate (`revoked_at`). SCIM endpoints are rate-limited and reject cross-org access.
- **Domain-verification trust:** auto-provisioning and email-linking trust a domain only after DNS verification, closing the "attacker claims acme.com and harvests JIT accounts" hole. This is the org-level analogue of `OAuthProvider::trusts_verified_email`.
- **Deprovisioning completeness:** deactivation flips `is_active` (the choke point `authenticate` already honors) and sweeps sessions + tokens, so revocation is not merely cosmetic.
- **Group-sync integrity:** `set_user_groups` is a full reconcile against the IdP claim set, so a removed IdP role removes the local group on next login; an app that needs local groups too uses additive mode.

### Admin surface (Section C)

`Org`, `OrgDomain`, `OrgMembership`, `ScimToken`, and `GroupMapping` register as admin models. A custom admin view shows per-org: verification status of each domain (with the TXT record to publish), the active/deprovisioned member roster, the SCIM token management (generate/revoke, secret shown once), and a group-mapping editor. `ScimToken` secrets render redacted.

---

## Phasing (honest sequencing)

The three plugins ship incrementally; each phase is independently useful and independently reversible.

1. **Phase 1: OAuth provider catalog + OIDC discovery.** Add `ProviderManifest`, first-class social providers beyond Google/GitHub (Apple, Microsoft, Facebook, X/Twitter, LinkedIn, GitLab, Bitbucket, Discord, Slack, Mastodon/custom OAuth2), and generic `.well-known` discovery with JWKS + ID-token verification. In `umbral-sso`, this covers Okta/Azure AD/Auth0/Google Workspace via OIDC; in `umbral-oauth`, it removes the "Google/GitHub only" adoption blocker.
2. **Phase 2: TOTP + recovery codes (`umbral-mfa`, core).** The login gate, `MfaFactor`/`RecoveryCode`, step-up, remembered devices, and `MfaPolicy` enforcement. Small, high-demand, no XML or WebAuthn complexity.
3. **Phase 3: OAuth Device Authorization Grant (`umbral-oauth`).** CLI/native-device login via `/oauth/device/*`, issuing named bearer tokens that appear in the #13 device inventory and honoring MFA/org policy during human approval.
4. **Phase 4: SCIM + JIT + group mapping + domain verification (`umbral-org`).** The lifecycle layer on top of Phases 1 and 2. JIT lands with Phase 1's identity resolution; SCIM and group mapping follow.
5. **Phase 5: SAML 2.0 (`umbral-sso`, `feature = "saml"`).** SP-initiated first (it has server-side `InResponseTo` state), IdP-initiated second and opt-in. Behind its own feature so OIDC-only apps never compile the XML-DSig stack. This is the largest single piece; it is sequenced last deliberately.
6. **Phase 6: WebAuthn/passkeys (`umbral-mfa`, `feature = "webauthn"`).** The registration/authentication ceremonies on top of a vetted `webauthn-rs`-style crate. Large and browser-coupled; sequenced after the TOTP path proves the gate.

Follow-ups explicitly not gating login: OAuth/OIDC token-refresh background jobs for provider API access (a natural `umbral-tasks` integration), and a distributed replay cache for SAML assertion IDs / OIDC nonces once multi-replica (ties into gaps5 #67 distributed throttling). A provider that advertises connect/API access should not call itself production-ready until refresh behavior is implemented or the manifest states `supports_refresh_token = false`.

## Cross-cutting security summary

| Threat | OIDC (A) | SAML (A) | MFA (B) | Org/SCIM (C) |
|---|---|---|---|---|
| Replay | server-side `nonce` | `Assertion@ID` cache + `InResponseTo` | single-use codes, TOTP step cache, WebAuthn counter | single-use hashed tokens |
| Forged signature | JWKS verify, reject `alg:none` | mandatory XML-DSig + wrapping guard | WebAuthn COSE verify | scoped `ScimToken` |
| Clock skew | bounded `exp`/`iat` window | bounded `NotBefore`/`NotOnOrAfter` | `±1` TOTP step | n/a |
| Downgrade | pin `S256`, match `aud`/`iss` | refuse unsigned, verify issuer | policy cannot be bypassed by recovery/remember | JIT only on verified domains |
| Session fixation | login via `umbral-sessions` (id rotation) | same | full session minted post-factor | same |
| Account takeover | trusted-email linking only | signed `NameID` only | step-up on sensitive actions | verified-domain gate before auto-link/JIT |

All row-level reads and writes go through the ORM (no raw `sqlx::query` in plugin code); the narrow allowed exceptions (schema DDL owned by migrations, backend-specific features) do not apply here. Every stored secret uses `Masked` or the `digest_token` hash-at-rest pattern already proven by `SocialAccount` and `AuthToken`.

## Open questions for the maintainer

1. Plugin naming: `umbral-sso` vs folding OIDC into `umbral-oauth` (OIDC genuinely IS OAuth + ID-token verification). The proposal keeps enterprise domain-routing, SAML, and SCIM-facing SSO in `umbral-sso`, while social OIDC providers and device flow stay in `umbral-oauth`. A shared internal OIDC crate with two public plugin surfaces is defensible.
2. Whether `umbral-org` should require `umbral-tenants` or keep the `Org`-to-`Tenant` link optional (the draft keeps it optional so single-tenant enterprise apps still get SCIM/JIT).
3. Group-sync default: full reconcile (IdP authoritative) vs additive-only. The draft defaults to full reconcile with a per-org opt-out.
4. Whether IdP-initiated SAML should exist at all given its weaker guarantees, or be dropped in favor of SP-initiated only.
