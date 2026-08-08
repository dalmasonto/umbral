# Unified authorization rule DSL and first-class webhook infrastructure (design)

Status: draft (planning/gaps5.md #38 tf#251, and #42 tf#255)
Date: 2026-08-08
Realizes Stage 2 (self-hosted platform posture) from `docs/decisions/2026-08-08-product-north-star.md`.

This document builds directly on two designs ratified the same day and does not restate them:

- `docs/decisions/2026-08-08-authorization-policy-design.md` - the `umbral-policy` plugin (the serializable `Expr` tree, `Policy`, `decide` / `explain`, `as_queryset_filter`, and the typed RLS builders). Read that first. Part 1 of this doc is the surface-spanning DSL layer that sits ON TOP of it, adding the storage-gate and realtime-channel compile targets to the two (REST scope, RLS) it already names.
- `docs/decisions/2026-08-08-cdc-outbox-and-read-replicas.md` - the `umbral-outbox` plugin (the durable `outbox_event` table, the `Destination` trait, the relay on `umbral-tasks`, at-least-once + idempotency key). Read that first. Part 2 of this doc is the webhook endpoint-management PRODUCT that uses that outbox relay as its delivery engine; it owns the `webhook` `Destination` the outbox doc explicitly deferred to #42.

Neither part changes the public contract of the plugin it layers over. Both are opt-in plugins; a REST-free, webhook-free app compiles and runs with zero code from either, per the thin-core rule.

---

# Part 1 (gaps5 #38): the surface-spanning authorization rule DSL

## What #38 asks for, against what the policy doc already delivers

gaps5 #38: "each subsystem has controls, but Firebase/Supabase users expect one mental model for auth rules across database/realtime/storage/functions. Define an authorization rule DSL or policy graph that compiles to REST scopes, RLS policies, storage gates, and realtime channel checks."

The authorization-policy design already built the engine and two of the four compile targets:

- The `Expr` tree and the `Policy` value are the "policy graph" - one serializable, typed predicate over `(subject, resource, action, context)`.
- `decide(subject, action, resource) -> Decision` is the single in-process decision seam. The policy doc already states that a custom handler, a storage-object gate, and a realtime channel-subscribe check all call it.
- `as_queryset_filter::<T>` lowers a read policy to a `Predicate<T>` for **REST list scoping**.
- The typed RLS builder (`RlsPolicy::owner/team/tenant`) lowers the same `Expr` to a Postgres `USING` / `WITH CHECK` clause for **RLS**.

What is missing, and what #38 is, is the last two compile targets plus the unifying framing: **one policy set, registered once, drives storage gates (`MediaAccessFn`) and realtime channel checks (`GroupPolicy`) too**, so the four surfaces stop hand-rolling their own logic. #38 is not a second engine. It is the two remaining compilers and the story that ties all four together into one mental model.

## The one mental model: a policy is `(subject, action, resource)`; a surface is a call site

Every one of the four surfaces reduces to the same question the policy doc's `decide` already answers: *is this subject allowed to perform this action on this resource, right now?* The differences between surfaces are only in **how the resource is named** and **when the check runs**, not in the rule. #38's job is to make each surface build the `(subject, action, resource)` triple from what it has in hand, then route through `decide` or one of the compilers.

| Surface | Existing seam | Resource is addressed by | How the policy reaches it |
|---|---|---|---|
| REST list | `umbral-rest` viewset queryset | the model `T` (many rows) | `as_queryset_filter::<T>` ANDed into the base queryset (already designed) |
| REST detail / write | `umbral-rest` viewset object hook | one row of `T` | `decide(subject, action, ResourceRef::row)` per object (already the `decide` seam) |
| Database | Postgres RLS | table + `current_setting` GUCs | typed RLS builder lowering (already designed) |
| Storage | `StoragePlugin::media_access` (`MediaAccessFn`) | a media key -> the owning row | **new: `PolicyMediaGate`, this doc** |
| Realtime | `RealtimePlugin` `GroupPolicy` (`can_join` / `can_send`) | a channel string -> a resource tuple | **new: `PolicyGroupPolicy`, this doc** |

The subject is resolved identically for all four: `umbral::auth::Identity` plus `umbral-permissions` group/permission membership, exactly as the policy doc's `Subject` already specifies. The context (tenant, MFA, time) comes from `RouteContext`. Nothing about the subject or the rule changes per surface; only the resource-addressing adapter does.

## New compile target 1: storage gates (`MediaAccessFn`)

`umbral-storage` today gates media with a `MediaAccessFn` - `Arc<dyn Fn(&HeaderMap, &str) -> Future<Output = bool>>` - set via `StoragePlugin::media_access(...)` / `media_access_identity(...)`, running on every `GET <mount>/<key>` before any bytes are served (`plugins/umbral-storage/src/lib.rs`). `media_access_owner()` is the one preset: resolve the caller, allow if the file's owner matches. That preset is exactly a one-off, hand-written instance of what a policy expresses generally.

#38 adds `umbral-policy`'s `PolicyMediaGate`, a constructor that produces a `MediaAccessFn` from the registered policy set:

```rust,ignore
StoragePlugin::new()
    .media("/media", "./media")
    // was: .media_access_owner() - a single hard-coded rule
    // now: every media request is decided by the same policy graph as REST/RLS
    .media_access(PolicyMediaGate::for_model::<Attachment>()
        .key_column("storage_key")          // the column holding the media key
        .action(ActionMatch::Read));
```

How it lowers. The gate is handed the request headers and the media key. It:

1. Resolves the `Subject` from the headers, the same resolution `media_access_identity` already performs (it reuses `umbral-auth`'s identity extraction, so the storage plugin gains no new auth dependency it did not already have via `media_access_identity`).
2. Resolves the `resource`: the media key identifies exactly one row. `PolicyMediaGate::for_model::<Attachment>().key_column("storage_key")` tells the gate to look the row up via the ORM - `Attachment::objects().filter(attachment::STORAGE_KEY.eq(key)).first()` - so the resource is a real row with real attributes (`owner_id`, `tenant_id`, `status`), not just an opaque key. This is a row read through the ORM, honoring the plugins-use-the-ORM rule; no raw SQL.
3. Calls `decide(&subject, Action::Read, &resource_ref).await.allowed`. The `Read` policies for `Attachment` - the *same policies that scope the REST list and the RLS predicate* - decide the media response. A deny returns the existing `403` / `forbidden_media()` shape; the storage plugin's 403 wiring is unchanged.

The result: `media_access_owner()` becomes the degenerate case of a policy `Expr::ResourceOwnedBy("owner_id")`. An app that writes one `Attachment.read` policy gets owner-scoping on the REST list, the RLS backstop, AND the media gate from that single rule, instead of writing the ownership check three times in three different shapes.

The honest limit, stated the way the policy doc states its RLS limit: a media gate is an app-layer check (there is no database-enforced backstop for a byte stream). Context-only predicates (time, MFA) enforce fine here because `decide` runs in-process with full `RouteContext`. A gate that cannot resolve a row for the key (orphaned key, deleted row) fails closed (deny), matching the default-deny posture.

## New compile target 2: realtime channel checks (`GroupPolicy`)

`umbral-realtime` today gates channel subscribe/publish with the `GroupPolicy` trait - `can_join(user_id, group) -> bool` and `can_send(user_id, group) -> bool` (`plugins/umbral-realtime/src/lib.rs`). The default `PublicGroupsOnly` denies any non-`public:` group. A real app overrides `can_join` to consult membership tables or tenant ids - again, a hand-written authorization check that duplicates rules living elsewhere. This is precisely the fragmentation gaps5 #45 also flags (realtime authz not unified with database/storage rules); #38 is the mechanism that closes #45.

#38 adds `PolicyGroupPolicy`, a `GroupPolicy` impl built from the policy set plus a channel-naming convention that maps a channel string to a `(resource, action)` pair:

```rust,ignore
RealtimePlugin::new()
    .with_auth_sessions()                        // user_id is the logged-in PK string
    // was: a bespoke .group_policy_async_fn(|uid, chan| { ... membership SQL ... })
    // now: channels are decided by the same policy graph
    .group_policy(PolicyGroupPolicy::new()
        // "post:42"  -> resource = Post row id=42, action = Read (join) / Update (send)
        .channel::<Post>("post", ChannelResource::row_by_pk())
        // "tenant:7" -> resource = tenant scope, action = Read
        .channel_scope("tenant", ContextKey::Tenant));
```

How it lowers. `GroupPolicy` already hands the check the authenticated `user_id` (the PK string) and the channel name; both `can_join` and `can_send` are `async`, so a DB-backed decision is a plain `.await` (the trait was made async in gaps4 #36 for exactly this). `PolicyGroupPolicy`:

1. Builds the `Subject` from `user_id` (the same PK-string shape the policy engine uses; anonymous is `None` -> the anonymous subject).
2. Parses the channel with the registered convention. `"post:42"` -> resource is `Post` row `id=42` (fetched via the ORM when the policy needs row attributes; a purely-ownership channel like `user:{id}` needs no fetch, the id is in the channel). `"tenant:7"` -> a context-scope resource keyed on the tenant.
3. Maps the direction to an action: **`can_join` -> `Action::Read`** (subscribing is reading the stream), **`can_send` -> the write action** (`Update` / a `Custom` verb per channel). This makes the read/write asymmetry the `GroupPolicy` doc-comment already discusses (a read-only broadcast channel; a post-but-not-subscribe room) fall out of the same read-vs-write policy split the REST and RLS surfaces use.
4. Returns `decide(&subject, action, &resource).await.allowed`.

An unparseable channel (no registered convention) falls back to the safe `PublicGroupsOnly` default - `public:*` allowed, everything else denied - so an un-modelled channel is never accidentally open. The result: the `tenant:99` isolation the `GroupPolicy` doc-comment warns about is now enforced by the same tenant policy that scopes REST and RLS, not a separately-maintained `can_join` body.

## The unifying registration: one policy set, four surfaces

The whole point is that the four adapters read the same registered policies. The wiring is one `PolicyPlugin` registration (unchanged from the policy doc) plus each surface opting its adapter in:

```rust,ignore
App::builder()
    .plugin(AuthPlugin::<AuthUser>::default().with_db_session_var("app.user_id"))
    .plugin(PermissionsPlugin::default())
    .plugin(
        PolicyPlugin::new()
            .policy(Policy::allow("post.read.same_tenant")
                .on::<Post>().action(ActionMatch::Read)
                .when(Expr::ResourceTenantMatches("tenant_id".into())))
            .policy(Policy::allow("post.edit.tenant_manager")
                .on::<Post>().action(ActionMatch::Update)
                .when(Expr::and([
                    Expr::InGroup("tenant_manager".into()),
                    Expr::ResourceTenantMatches("tenant_id".into()),
                ]))),
    )
    // RLS backstop: the SAME policies lower to Postgres USING/WITH CHECK
    .plugin(RlsPlugin::from_policies())            // Part-2/#17 typed builders
    // Storage: the SAME Read policies gate media bytes
    .plugin(StoragePlugin::new().media("/media", "./media")
        .media_access(PolicyMediaGate::for_model::<Attachment>().key_column("storage_key")))
    // Realtime: the SAME policies gate channel join/send
    .plugin(RealtimePlugin::new().with_auth_sessions()
        .group_policy(PolicyGroupPolicy::new().channel::<Post>("post", ChannelResource::row_by_pk())))
    // REST: the viewset ANDs as_queryset_filter into every list
    .plugin(RestPlugin::new().scoped_by_policy())
    .build()?;
```

Write the `post.read.same_tenant` rule once; it scopes the REST list (`as_queryset_filter`), backstops at the database (RLS), gates the media bytes attached to a post (`PolicyMediaGate`), and gates the realtime `post:*` channel (`PolicyGroupPolicy`). That is the "one mental model across database, realtime, storage, functions" #38 asks for, delivered as four thin adapters over the one `decide` / `as_queryset_filter` engine the policy doc already built.

## Boot-time coherence and the lowering-gap report

The policy doc's `on_ready` already validates every policy against `ModelMeta` (fail boot on an unknown column/model). #38 extends the same boot check across the adapters:

- A `PolicyMediaGate::for_model::<T>().key_column(c)` whose `c` is not a column of `T`, or whose `T` has no `Read` policy at all, fails boot (a media mount silently allowing everything is the storage plugin's existing "gated mount with no rule" warning, now promoted to a policy-coherence error).
- A `PolicyGroupPolicy` channel prefix mapped to a model with no matching policy is reported at boot.
- The policy doc already notes that only the ORM-expressible subset of `Expr` lowers to a `Predicate<T>` / RLS, and that the compiler *reports* which policies could not be pushed down rather than silently dropping them. #38 unifies that report across all four surfaces: `umbral policy targets` prints, per policy, which of {REST-filter, RLS, storage-gate, realtime-channel} it compiles to and which it can only enforce in-process via `decide`. A context-only predicate (`context.hour in 9..18`) enforces at REST-detail / storage / realtime (all call `decide`) but not at the RLS or REST-list layer, and the report says so. This is the same honesty the policy doc applies to RLS lowering, generalized to four targets.

## `explain` spans surfaces too

The policy doc's `explain(subject, action, resource) -> Explanation` and its `umbral policy explain --subject alice --action update --resource post:42` already answer "would alice be allowed, and why". Because a storage request and a realtime subscribe both reduce to `(subject, action, resource)`, the same `explain` answers "why was this media request denied" and "why can't alice join `post:42`" with no new machinery - the CLI just takes a `--resource media:<key>` or `--resource channel:post:42` that the adapters resolve to the same triple. One dry-run tool for all four surfaces.

## What Part 1 deliberately does not do

- It does not add a second policy engine or a new predicate language. `Expr`, `Policy`, and `decide` are the policy doc's; #38 is two adapters (`PolicyMediaGate`, `PolicyGroupPolicy`) plus the cross-surface boot check and target report.
- It does not change `MediaAccessFn` or `GroupPolicy`. Both stay exactly as shipped; the new types are ordinary implementations of them, so a hand-written gate/policy keeps working and an app can mix (policy-driven for most models, a bespoke `MediaAccessFn` for one).
- It does not give storage or realtime a database-enforced backstop. Only RLS has that (Postgres-only, per the policy doc). Storage and realtime are app-layer checks, stated plainly.

---

# Part 2 (gaps5 #42): first-class webhook infrastructure

## What #42 asks for, against what the outbox doc already delivers

gaps5 #42: "no durable webhook sender/receiver plugin was found. Add signed webhooks, retries, delivery logs, endpoint secrets, replay, per-tenant quotas, and admin UI."

The CDC/outbox design already delivers the **delivery engine**: the `umbral-outbox` plugin, its durable `outbox_event` table, the relay on `umbral-tasks` with exponential backoff (`retry_backoff_base * 2^(attempts-1)`, capped, dead-letter ceiling - the same model `umbral-tasks` uses), the per-attempt delivery log (`outbox_delivery`), the at-least-once guarantee with the event `id` as idempotency key, and the pluggable `Destination` trait. Crucially, the outbox doc ships a `webhook` `Destination` that "POSTs the event payload to a URL" but **explicitly defers endpoint management (which URLs, per-tenant, secrets/HMAC signing, replay, quotas, admin UI) to #42**, and says the webhook destination takes a static URL + shared secret from settings until #42 lands.

So #42 is not a delivery engine. It is the **webhook-as-a-product** layer: an endpoint registry, secrets and HMAC signing, subscription filtering, replay, per-tenant quotas, and the admin UI - all riding the outbox relay as the send-with-retry-and-log substrate. The relationship is exact: **#31 produces the event and delivers-with-retry; #42 owns *where* the event is delivered, *how* it is signed, and *who* administers the endpoints.**

## The `umbral-webhooks` plugin

A new built-in plugin, `plugins/umbral-webhooks`, depending on the `umbral` facade plus `umbral-outbox` (for the relay + `Destination` trait it plugs into) and, optionally, `umbral-admin` (for the UI). It contributes three models and one `Destination` implementation, and it is the concrete `webhook` destination the outbox doc left as a placeholder.

### Model 1: the endpoint registry (`WebhookEndpoint`)

One row per registered receiver, owned by the plugin, migrated the normal way (`plugin.migrations()`):

| Column | Type | Meaning |
|---|---|---|
| `id` | PK | endpoint id (PK-shape independent: i64/String/Uuid) |
| `tenant_id` | Option<String> | owning tenant (NULL = a global/system endpoint); the quota + admin-scoping key |
| `url` | String | the HTTPS target the relay POSTs to |
| `secret` | Masked<String> | the HMAC signing secret, encrypted at rest via the `Masked<T>` field type |
| `events` | Json | subscribed event patterns, e.g. `["order.created", "order.*", "*"]` - matched against `outbox_event.aggregate` + `event_type` |
| `active` | bool | soft on/off without deleting history |
| `description` | String | operator-facing label |
| `created_at` / `updated_at` | DateTime | audit timestamps |

The secret is `Masked<String>` (the field-level encryption shipped in the Masked + oauth build), so the signing key is encrypted at rest and never rendered in the admin - the admin shows only "set / rotate", never the plaintext. Endpoints are created/edited through the ORM (`WebhookEndpoint::objects().create(...)`), never raw SQL.

### Model 2: the delivery attempt log (`WebhookDelivery`)

The outbox already writes an `outbox_delivery` row per `(event, destination, attempt)`. #42's `WebhookDelivery` is the webhook-specific projection the product needs - one row per attempt against a specific endpoint, so an operator sees "endpoint X, event Y, attempt 3, 503, 812ms":

| Column | Type | Meaning |
|---|---|---|
| `id` | PK | delivery-attempt id |
| `endpoint_id` | FK -> WebhookEndpoint | which endpoint |
| `event_id` | FK -> OutboxEvent | which source event (the idempotency key the receiver dedupes on) |
| `attempt` | i32 | attempt number (mirrors the outbox relay's `attempts`) |
| `request_signature` | String | the HMAC signature header sent (for support/debugging; not the secret) |
| `status_code` | Option<i32> | HTTP status the receiver returned (NULL = transport error) |
| `error` | Option<String> | transport/timeout error, if any |
| `latency_ms` | i32 | round-trip latency |
| `delivered_at` | DateTime | attempt timestamp |
| `outcome` | String | `"success" \| "retrying" \| "dead_letter"` |

This is the delivery-log surface #42 asks for, keyed to endpoints (the outbox's `outbox_delivery` is keyed to destinations generically; this narrows to the webhook product's view). It is written through the ORM by the webhook `Destination`.

### Model 3: per-tenant quota counters (`WebhookQuota`)

A small counter model per `(tenant_id, window)` the relay consults before delivering, so a noisy tenant cannot exhaust the shared relay:

| Column | Meaning |
|---|---|
| `tenant_id` | the tenant |
| `window_start` | the rate-limit window (e.g. rounded to the minute/hour) |
| `count` | deliveries attempted in the window |
| `limit` | the tenant's ceiling (from settings or a plan) |

When a tenant is over quota, the relay does not drop the event - it pushes `available_at` forward (the same backoff mechanism the outbox already uses for a failed delivery), so quota is a throttle, not data loss. Over-quota events stay durable in `outbox_event` and drain when the window resets.

## HMAC signing: the `WebhookDestination`

The webhook `Destination` (`impl Destination for WebhookDestination`) is what the outbox relay calls for the `webhook` destination name. Its `deliver(&self, event: &OutboxEvent)`:

1. **Resolves endpoints.** Looks up every `active` `WebhookEndpoint` whose `events` pattern matches this event's `aggregate` + `event_type` AND whose `tenant_id` matches the event's tenant (a per-tenant event fans out only to that tenant's endpoints; a global event to global endpoints). This is the outbox doc's stated contract - "the webhook `Destination` calls into #42's endpoint registry to resolve targets and sign requests" - made concrete. One outbox event fans out to N endpoints; each endpoint delivery is its own `WebhookDelivery` row with its own retry lineage.
2. **Signs.** Computes `HMAC-SHA256(secret, timestamp + "." + body)` and sends it as `X-Umbral-Signature: t=<ts>,v1=<hex>` plus `X-Umbral-Event-Id: <event.id>` (the idempotency key) and `X-Umbral-Event-Type`. The timestamp is inside the signed payload so a receiver rejects replayed captures outside a tolerance window - the standard signed-webhook scheme (Stripe-style `t=,v1=`). The secret is decrypted from `Masked<String>` only in-memory at send time.
3. **POSTs and records.** Sends the request, writes a `WebhookDelivery` row, and returns `Ok`/`Err(DeliveryError)` to the relay. On `Err`, the outbox relay's existing backoff + dead-letter machinery takes over - **#42 writes zero retry logic; the retry is the outbox relay's, unchanged.** A 2xx is success; a 4xx (except 429) is a permanent failure (bad endpoint config, dead-lettered fast); a 5xx / 429 / timeout is retriable.

We ship a receiver-side verification helper (`umbral_webhooks::verify_signature(secret, headers, body) -> bool`) so an umbral app *receiving* another umbral app's webhooks - or any consumer - can validate the HMAC and reject stale timestamps without re-implementing the scheme. This is the "receiver" half #42's evidence line mentions.

## Replay

Because every delivered event is a durable `outbox_event` row (retained for the outbox's configurable window, default 7 days) and every attempt is a `WebhookDelivery` row, replay is a re-enqueue, not a re-derivation:

- **`umbral webhooks replay --event <id> [--endpoint <id>]`** re-submits an existing `outbox_event` to the webhook destination for one or all matching endpoints, writing a fresh `WebhookDelivery` (with a new attempt lineage) and re-signing with the current secret. The receiver dedupes on `X-Umbral-Event-Id` if it has already processed it - the at-least-once + idempotency-key contract the outbox already documents makes replay safe by construction.
- **`umbral webhooks replay --endpoint <id> --since <ts>`** replays every event an endpoint should have received in a window - the recovery path for "my endpoint was down for an hour". This reads `outbox_event` (the durable log), so it works even if the original delivery attempts all dead-lettered.

Replay is exposed in the admin too (a "resend" button per delivery / per endpoint).

## Per-tenant quotas

Beyond the `WebhookQuota` throttle above, `WebhookPlugin` exposes the operator knobs:

```rust,ignore
WebhookPlugin::new()
    .max_endpoints_per_tenant(20)          // registry-size cap
    .delivery_rate_per_tenant(600, Duration::minutes(1))   // relay throttle -> WebhookQuota
    .max_attempts(12)                      // hands through to the outbox relay's dead-letter ceiling
    .signature_tolerance(Duration::minutes(5));  // receiver-side timestamp window
```

The delivery-rate cap is enforced in the `WebhookDestination` before the POST (consult `WebhookQuota`, push `available_at` if over), so it rides the relay's existing scheduling rather than adding a second rate limiter. The endpoint-count cap is enforced at registry-write time (in the admin/API create path), failing the create with a clear error rather than silently.

## Admin UI

The admin surface is an `AdminPlugin::view(AdminView)` custom view (the admin custom-view seam already shipped), tenant-scoped via the same `Widget::permission` enforcement custom views already carry, so a tenant admin sees only their own endpoints:

- **Endpoints list + editor**: register a URL, pick subscribed event patterns from the known `outbox_event` aggregates/types, rotate the secret (write-only; the plaintext is shown once at generation and never again, backed by `Masked<String>`), toggle `active`.
- **Delivery log**: a filterable `WebhookDelivery` table (by endpoint, event type, outcome, time), each row showing status/latency/attempt and a "resend" button.
- **Health at a glance**: per-endpoint success rate and consecutive-failure count over a recent window (a projection over `WebhookDelivery`), so an operator spots an endpoint that has started 5xx-ing. An endpoint past a consecutive-failure threshold is flagged (and optionally auto-`active=false` with a notice), the standard "disable a persistently-failing webhook" behavior.
- **Test-send**: fire a synthetic `webhook.ping` event to one endpoint to verify signing + reachability before real events flow.

Charts in the admin view use ApexCharts and icons use Lucide, per the standardized-libraries convention - no hand-rolled SVG.

## Why this shape

- **It stands entirely on the outbox relay.** The durable send, backoff, dead-letter, at-least-once, idempotency key, and per-attempt log are all `umbral-outbox`'s, already designed and built on `umbral-tasks`. #42 adds three models, one `Destination` impl, HMAC signing, replay/quota CLIs, and an admin view - it writes no retry loop, no scheduler, no queue.
- **It closes the loop the outbox doc opened.** The outbox doc names the `webhook` `Destination` a placeholder taking a static URL + shared secret "until #42 lands". This is #42 landing: the placeholder becomes a registry-backed, per-tenant, signed, replayable, quota'd, admin-managed destination, and the outbox doc's deferral is satisfied exactly as written.
- **Secrets are safe by construction.** `Masked<String>` encrypts the signing secret at rest and keeps it out of the admin render path; the plaintext exists only in memory at sign time and once at generation. No new secret store is invented.
- **`umbral-core` stays plugin-free.** `umbral-webhooks` is a plugin depending on the `umbral` facade plus sibling plugins (`umbral-outbox`, `umbral-admin`); core never learns webhooks exist. If any of this could not be a plugin, the plugin contract would be wrong.

## Deferred / out of scope for #42

- **The event production itself** - that is #31 (the outbox). #42 owns endpoint management, signing, and delivery-record product surface, not what generates events.
- **Exactly-once delivery** - inherited from the outbox: at-least-once + the `X-Umbral-Event-Id` idempotency key. Receivers dedupe; #42 does not promise exactly-once.
- **A hosted webhook-management API / Terraform provider** - that is gaps5 #39 (management API / IaC), which lists webhooks as one of its resources. #42 ships the in-app registry + admin + CLI; the external control-plane API is #39's job.
- **Non-HTTP destinations** (Kafka, SQS) - those are plain outbox `Destination`s an app adds directly (`OutboxPlugin::destination(...)`); they do not need the webhook endpoint-registry product.

---

## Summary of the contract

- **#38:** two new adapters over the already-designed `umbral-policy` engine - `PolicyMediaGate` (a `MediaAccessFn` built from the policy set, resolving a media key to its owning row via the ORM and deciding via `decide`) and `PolicyGroupPolicy` (a `GroupPolicy` mapping a channel string to a `(resource, action)` and deciding via `decide`, with `can_join`->`Read` and `can_send`->write) - so one registered policy set drives all four surfaces: REST scoping (`as_queryset_filter`), RLS (typed builders), storage gates, and realtime channels. Boot-time coherence checks and a per-policy target report ("this rule compiles to REST-filter + RLS + storage + realtime; that one is in-process only") make the lowering honest across all four. This is the one mental model gaps5 #38 asks for, and it is the mechanism that also closes #45.
- **#42:** a new `umbral-webhooks` plugin that turns the outbox doc's placeholder `webhook` `Destination` into a product - a `WebhookEndpoint` registry (url, `Masked` secret, subscribed event patterns, tenant, active), HMAC-SHA256 signing with a timestamped `t=,v1=` header and an event-id idempotency key, a `WebhookDelivery` per-attempt log, `WebhookQuota` per-tenant throttling, `umbral webhooks replay` (event/endpoint/window), and a tenant-scoped admin custom view (endpoints, delivery log, health, test-send). All delivery, retry, backoff, dead-lettering, and at-least-once semantics ride the `umbral-outbox` relay unchanged; #42 owns where events go, how they are signed, and who administers them, not the sending itself.
