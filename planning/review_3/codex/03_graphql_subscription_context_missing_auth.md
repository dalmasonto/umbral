# GraphQL Subscriptions Miss Query Context

Category: Correctness, Security
Severity: Medium

## Finding

GraphQL POST requests inject loaders, private unlocks, and optional identity into the request context. The subscription transports do not appear to inject equivalent context.

This looks fail-closed for private fields and auth-gated subscription guards, but it makes subscriptions behave differently from queries and mutations. It can also encourage apps to weaken subscription guards because identity is absent.

## Evidence

- `plugins/umbral-graphql/src/lib.rs:556-585` injects `Loaders`, `PrivateUnlocks`, and `Option<Identity>` for POST GraphQL requests.
- `plugins/umbral-graphql/src/lib.rs:614-637` wires WebSocket and SSE subscription routes without the same context assembly.
- `plugins/umbral-graphql/src/subscription.rs:165-180` evaluates guards and privacy through GraphQL request context.

## Risk

Authenticated subscriptions and private field unlocks may not work, even when the equivalent POST query works. That can break correctness for real-time features and create pressure to make subscription guards public.

## Recommendation

Create one shared context builder for GraphQL HTTP, WebSocket, and SSE entry points. It should consistently attach identity, loaders, private unlocks, tenant/session data, and any future GraphQL policy context.

## Suggested Tests

- A subscription with `expose_if` requiring an authenticated identity succeeds over WS and SSE when authenticated.
- The same subscription is denied when unauthenticated.
- Private fields have the same behavior over POST, WS, and SSE.

