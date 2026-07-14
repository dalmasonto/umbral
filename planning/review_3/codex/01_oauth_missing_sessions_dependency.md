# OAuth Requires Sessions But Does Not Declare It

Category: Correctness, Security
Severity: High

## Finding

The OAuth plugin routes require `SessionToken`, but `OAuthPlugin::dependencies()` only declares `auth`. An app can register OAuth without sessions, pass plugin dependency validation, and then hit extractor failures on login and callback routes.

## Evidence

- `plugins/umbral-oauth/src/lib.rs:203-212` declares only `&["auth"]`.
- `plugins/umbral-oauth/src/routes.rs:196-207` and `plugins/umbral-oauth/src/routes.rs:209-228` require `Extension(SessionToken(token))`.
- `plugins/umbral-sessions/src/lib.rs:1340-1423` is the middleware path that injects `SessionToken`.

## Risk

OAuth sign-in can fail at runtime even though the app builds successfully. This is also a security-sensitive path, because partial OAuth registration failures tend to get diagnosed under production pressure.

## Recommendation

Declare `sessions` as an OAuth dependency and update the OAuth examples/docs to register `SessionsPlugin`. Add a boot check that fails clearly if OAuth routes are enabled without session middleware.

## Suggested Tests

- Build an app with `AuthPlugin + OAuthPlugin` but no `SessionsPlugin`; assert build fails with a dependency error.
- Build an app with all three plugins; assert `/auth/oauth/:provider/start` and callback routes receive a session token.

