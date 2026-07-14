# Storage Media Is Public By Default

Category: Security
Severity: Medium

## Finding

The storage plugin serves media publicly unless `media_access` is configured. The docs warn that private uploads require explicit policy, but the default remains easy to use unsafely.

## Evidence

- `plugins/umbral-storage/src/lib.rs:51-57` documents optional access control.
- `plugins/umbral-storage/src/lib.rs:164-179` exposes `media_access` as optional.
- Storage routes serve uploaded media unless a configured policy denies access.

## Risk

Applications that store user uploads may accidentally expose private documents, profile images, invoices, or tenant files by relying on the default route.

## Recommendation

Keep backward compatibility if needed, but add stronger guardrails:

- Production warning when media serving is enabled with no `media_access`.
- A config option that declares the media mount as public/private.
- Examples that use explicit access control for user-owned uploads.

## Suggested Tests

- Production build with media route and no access policy emits a warning.
- A private media policy blocks unauthenticated access.
- A public media policy still permits intended public assets.

