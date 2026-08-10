# EnterprisePreset: one switch for the production posture (design)

Status: draft (planning/gaps5.md #3, tf #216)
Date: 2026-08-08
Realizes Stage 2 (self-hosted platform posture) from `docs/decisions/2026-08-08-product-north-star.md`.

## Problem

The hardened pieces already exist, but they are wired separately. Security headers and CSRF are on by default (the scaffold mounts `SecurityPlugin`), but sessions, auth, health, logs, RLS, permissions, and the coming metrics and distributed throttling are each opt-in, and the production-safe configuration (HSTS, CSP, secure cookies, host validation, trusted-proxy handling) is left to the adopter to assemble and remember. There is no single switch that says "give me the expected production posture, and fail boot if something is unsafe."

## Goal

One opt-in that installs the secure production stack and adds production system checks. This is composition, not new privilege: the preset is a bundle of existing plugins plus configuration, consistent with the motto (every capability is a plugin, including auth). Removing the preset and wiring the same plugins by hand must be equivalent.

## Shape (recommended)

Expose the same underlying bundle two ways:

1. **Builder:** `App::builder().preset(EnterprisePreset::default()).build()`. A `Preset` is a value that, given a builder, registers plugins and sets config. `EnterprisePreset` registers `SecurityPlugin` (HSTS on, a starter CSP, frame `DENY`), `SessionsPlugin` (secure cookies, `SameSite`, max-age), `AuthPlugin`, `HealthPlugin` (readiness and liveness), `LogsPlugin` (request logs), and, when the crates are present, a metrics exporter (gaps5 #64) and a distributed throttle backend (gaps5 #67). It also enables trusted-proxy handling and host validation.
2. **Scaffold flag:** `umbral startproject --prod-hardening` (and/or `--profile ...`, ties to gaps5 #82) generates a `main.rs` that calls the preset, so the wiring is visible and editable rather than hidden.

**Packaging.** Put `EnterprisePreset` in a small `umbral-enterprise` meta-crate that depends on the security-relevant plugins, rather than in the facade. This preserves the dependency-inversion rule (the `umbral` facade keeps zero plugin dependencies). Recommended over a facade feature module for that reason.

### Identity hardening profile

The base preset wires the shipped auth/session/security posture. It must also make the broader identity roadmap visible, because production adopters will otherwise think "auth is installed" means "enterprise identity is done". Add an optional identity profile inside the meta-crate:

```rust
EnterprisePreset::default()
    .identity(IdentityPreset::recommended())
```

`IdentityPreset::recommended()` does not force every app to support every provider. It composes what the app has configured and emits system-check findings for missing production pieces:

- **Session/device inventory.** When gaps5 #13 lands, require bearer-token max age, device/session listing, revoke-other-devices, and admin revoke actions for apps that mint long-lived bearer tokens or enable OAuth device flow.
- **OAuth/social providers.** When `umbral-oauth` is mounted, validate HTTPS redirect bases, provider credentials, PKCE/state enabled, provider manifests present, and untrusted email providers not allowed to auto-link accounts. The provider catalog comes from `docs/decisions/2026-08-08-enterprise-identity-design.md`: Google/GitHub plus Apple, Microsoft, Facebook, X/Twitter, LinkedIn, GitLab, Bitbucket, Discord, Slack, and custom OAuth2/OIDC as configured.
- **Enterprise SSO.** When `umbral-sso` is mounted, require OIDC issuer/audience/JWKS checks, domain mappings only for verified domains, SAML signature enforcement, and IdP-initiated SAML explicitly opted in.
- **MFA.** Recommend `MfaPolicy::RequiredForStaff` at minimum; fail only when the app explicitly marks itself enterprise-hardened and staff MFA is absent. TOTP/recovery codes are the first required factor set; WebAuthn/passkeys are recommended once shipped.
- **Client attestation.** When `umbral-attest` is mounted, start in Monitor mode and require an explicit `Enforce` decision per route group. Absence of attestation is a warning for public mobile/API surfaces, not a hard fail for ordinary server-rendered apps.

This keeps the preset honest: username/password auth is a safe baseline, but production readiness for a SaaS or BaaS-shaped deployment also needs the #9/#10/#12/#13 identity layers.

## Production system checks (fail boot, not prod)

Add checks that run at boot under `Environment::Prod` and fail with a clear, actionable message:

- `secret_key` is not the default or empty.
- Dev mode / debug is off.
- Cookies are `Secure` and HSTS is on when serving over TLS.
- Host validation / allowed hosts is configured.
- A trusted-proxy list is set when behind a proxy (so client IP and rate limiting are correct).
- The database is Postgres (SQLite emits a not-for-production warning).
- Bearer tokens have a max age when API auth is enabled.
- OAuth/SSO providers use HTTPS redirect/callback origins outside `Dev` and are not half-configured.
- Staff MFA is configured or explicitly waived when the identity hardening profile is enabled.

These extend the existing boot-time system-check mechanism rather than adding a parallel one.

## What this is NOT

Not a new privileged path. Every piece is a plugin an app can add or remove individually; the preset is convenience plus guardrails. It does not hide behaviour that the plugins do not already provide.

## Dependencies and follow-ups

- Metrics (gaps5 #64) and distributed throttling (gaps5 #67) must land before the preset can include them; until then the preset emits a warning that they are recommended for multi-replica production.
- Enterprise identity pieces (gaps5 #9, #10, #12, #13) remain separately tracked. The preset should compose and check them when present; it should not hide the fact that SSO/MFA/device inventory/attestation are separate implementation tasks.
- Ties to scaffold profiles (gaps5 #82) and `SECURITY.md` (gaps5 #20).

## Open decision for the maintainer

`umbral-enterprise` meta-crate (recommended) versus a facade feature module. The meta-crate keeps the facade plugin-free; the feature module is one less crate. Recommend the meta-crate.
