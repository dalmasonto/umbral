# Analytics Auto Pageviews Send Raw Paths

Category: Privacy, Security
Severity: Medium

## Finding

The analytics plugin can automatically capture pageviews and send the raw request path. Prefix exclusion exists, but there is no default scrubber for IDs, emails, invite tokens, reset tokens, tenant slugs, or other sensitive path segments.

## Evidence

- `plugins/umbral-analytics/src/lib.rs:274-305` documents automatic pageview capture.
- `plugins/umbral-analytics/src/lib.rs:134-165` provides configurable excluded prefixes.
- The pageview payload includes route path-like request metadata.

## Risk

URLs often contain sensitive or identifying data. Sending raw paths to third-party analytics can leak personal data, secrets, tenant identifiers, or private object IDs.

## Recommendation

Add path privacy controls:

- Default scrubber for UUIDs, numeric IDs, emails, and token-like segments.
- Option to report route names/templates instead of raw paths.
- Default exclusions for auth, admin, media, API, and webhook paths unless opted in.

## Suggested Tests

- `/reset/abcdef-token` is captured as `/reset/[token]` or excluded.
- `/users/user@example.com` does not send the email.
- Apps can opt into raw paths only explicitly.

