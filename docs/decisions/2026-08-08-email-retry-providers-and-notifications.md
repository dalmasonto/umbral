# Durable email, broader providers, and unified notifications (design)

Status: draft for ratification (proposes the shape of gaps5 #53, #54, #55)
Date: 2026-08-08
Relates: planning/gaps5.md #53 (tf#266, durable email retry queue), #54 (tf#267, broader provider support), #55 (tf#268, unified notifications)

This document builds on two designs ratified the same day and does not restate them:

- `docs/decisions/2026-08-08-cdc-outbox-and-read-replicas.md` - the `umbral-outbox` plugin (the durable `outbox_event` table, the `Destination` trait, the relay on `umbral-tasks`, at-least-once delivery with the event `id` as idempotency key, exponential backoff, the dead-letter ceiling, and the per-attempt `outbox_delivery` log). Read that first. #53 routes email through that relay.
- `docs/decisions/2026-08-08-unified-authz-dsl-and-webhooks.md` - the `umbral-webhooks` plugin (HMAC signing, `verify_signature`, replay, the endpoint registry, per-attempt `WebhookDelivery` log). #54's inbound bounce/complaint webhooks reuse its receiver-side verification helper.

All three parts are opt-in plugins layered on the existing `umbral-email` plugin (`plugins/umbral-email/src/lib.rs`). A framework app that only wants a single fire-and-forget `send(&EmailMessage)` keeps working with zero code from any of them, per the thin-core rule.

## What exists today, stated precisely

`umbral-email` (`plugins/umbral-email/src/lib.rs`) is a service-shaped plugin that contributes no models, no routes, no system checks. Its surface:

- `EmailMessage` - a builder-shaped message (`from`, `to: Vec<String>`, `subject`, `text_body`, `html_body`, `reply_to`, `attachments: Vec<Attachment>`).
- `send(&EmailMessage) -> Result<(), EmailError>` - resolves the `from` (message field, then the `email_default_from` setting, then `FALLBACK_FROM`), validates header values against the RFC 5322 injection guard (`validate_header_value`), then dispatches to one of three backends.
- Backend selection in `load_config`: `UMBRAL_EMAIL_BACKEND=console` forces console; else API (env-forced, or `email_api_provider` + `email_api_key` both set); else SMTP (`email_smtp_host` set); else console.
- The API backend today supports exactly two providers, the `EmailApiProvider` enum: `Resend` (`POST https://api.resend.com/emails`) and `SendGrid` (`POST https://api.sendgrid.com/v3/mail/send`). `build_api_request(provider, key, msg, default_from) -> ApiRequest` is a pure function mapping an `EmailMessage` into the provider's JSON shape and endpoint; `deliver_api(request)` does the `reqwest` POST under the `api` cargo feature.
- `EmailError` carries `Smtp`, `ApiTransport`, `ApiResponse { status, body }`, and the config errors. Its doc-comment on the `Smtp` variant already states: "v1 does not retry; callers wanting durability enqueue the send via `umbral-tasks`."

The module docstring (`lib.rs:44-58`) names the exact gaps this doc closes: "No retry queue. Transient SMTP failures bubble up as `EmailError::Smtp`. Wiring this through `umbral-tasks` lands in a future round (`enqueue("send_email", payload)`)." And: "The API backend POSTs JSON to a transactional-email provider (Resend / SendGrid) and complements - does not replace - SMTP."

So the three parts of this doc are the three sentences the plugin already promised: durable retry via the queue (#53), more providers behind the same seam (#54), and the notification layer on top (#55).

---

## Part 1 (#53): durable email through the queue and outbox

### The problem, stated against what exists

`send(&EmailMessage)` is a single synchronous handoff. A transient SMTP failure (`EmailError::Smtp`), an API 5xx (`EmailError::ApiResponse { status: 503, .. }`), or a network blip (`EmailError::ApiTransport`) all bubble straight up to the caller with no retry. A handler that calls `send(...)?` in the request path either loses the mail on a transient provider hiccup, or blocks the request while it waits on a 10-second SMTP timeout. Password resets and welcome mails - the exact flows the console-backend design cites as the footgun to avoid - are one relay outage away from being dropped.

The durable-retry machinery already exists in two shipped plugins and does not need reinventing:

- `umbral-tasks` gives an at-least-once queue with `enqueue`, `#[task]`, exponential backoff (`retry_backoff_base * 2^(attempts-1)`, capped at `retry_backoff_max`), a `max_attempts` ceiling with a dead-letter terminal state, and status readback by task id.
- `umbral-outbox` gives the transactional guarantee: an outbox row written inside the business transaction commits atomically with the business write, and the relay publishes it after commit, at-least-once, with the event `id` as idempotency key.

#53 is the composition of those two with the email plugin. It adds no new retry loop and no new scheduler.

### The design: an `email` task plus an outbox destination

#### The `#[task]` send path (the queue integration)

A new durable send entry point that enqueues instead of blocking:

```rust,ignore
// was: umbral_email::send(&msg).await?;   // synchronous, no retry
// now: enqueue a durable, retried send
umbral_email::send_durable(&msg).await?;   // returns a delivery id, never blocks on the relay
```

`send_durable` serializes the `EmailMessage` into a task payload and calls `umbral-tasks`' `enqueue`. The registered `#[task] email_send` handler is the thin wrapper around the existing `send`:

```rust,ignore
#[umbral::task]
async fn email_send(payload: EmailSendPayload) -> Result<(), TaskError> {
    let msg = payload.into_message();
    umbral_email::send(&msg).await.map_err(email_error_to_task_error)?;
    Ok(())
}
```

The mapping from `EmailError` to the task's retriable/permanent split is the load-bearing piece, and it reuses the classification `umbral-webhooks` already applies to HTTP status:

- **Retriable** (the handler returns `Err`, the queue backs off and retries): `EmailError::Smtp` (connection / TLS / relay temporary reject), `EmailError::ApiTransport` (DNS / connection / timeout), and `EmailError::ApiResponse { status, .. }` where `status` is 5xx, 429, or 408. A 4xx SMTP reject that is transient (421, 450, 451, 452 greylisting) is retriable too.
- **Permanent** (dead-letter immediately, no point retrying): `EmailError::MissingFrom`, `EmailError::NoRecipients`, `EmailError::InvalidHeaderValue`, `EmailError::InvalidAttachmentContentType`, `EmailError::ApiNotConfigured`, `EmailError::ConsoleBackendInProduction`, and `EmailError::ApiResponse` with a 4xx other than 429/408 (bad key, malformed payload, rejected recipient). These are configuration or data errors that a retry cannot fix; failing fast surfaces them to the operator instead of burning the whole `max_attempts` budget.

To make the queue honor "permanent" without a retry, the handler distinguishes the two by returning a `TaskError` the worker treats as terminal for the permanent set (the queue's existing terminal-vs-retriable `TaskError` distinction), so a bad-recipient send dead-letters on attempt 1 while a 503 rides the full backoff schedule.

#### The transactional path (the outbox integration)

For the strong guarantee - "send this mail if and only if the order commits" - the send rides the outbox, which is the blessed after-commit path from the CDC/outbox doc. An `email` `Destination` (`impl Destination for EmailDestination`) is registered on `OutboxPlugin`, and the business code writes an email intent into the outbox inside its transaction:

```rust,ignore
umbral::db::transaction(|tx| Box::pin(async move {
    let order = Order::objects().on_tx(tx).create(new_order).await?;
    // the email intent commits atomically with the order row
    outbox::publish_on(tx, Event::to_email("order.confirmation", &order)).await?;
    Ok::<_, MyError>(order)
})).await?;
```

The outbox relay, on its next run, hands the event to the `email` `Destination`, which materializes the `EmailMessage` (from a template plus the event payload) and calls `send`. The relay owns the backoff, the dead-letter ceiling, and the per-attempt `outbox_delivery` log. Because the outbox row and the order commit together, there is no window where the order exists without the queued mail, and no window where the mail is queued for an order that rolled back. This is the dual-write problem solved exactly as the outbox doc describes; email is simply its first named consumer (`#53` is the "email (#53)" the outbox doc points forward to).

The two paths are complementary, not redundant: `send_durable` is the low-ceremony "retry this, I do not need it tied to a transaction" case (a welcome mail after signup); `outbox::publish_on(tx, ..)` is the "this mail must be exactly as durable as the row that triggered it" case. Both terminate in the same `send`.

### The delivery-status model (`EmailDelivery`)

Neither the queue's `TaskRow` nor the outbox's generic `outbox_delivery` is an email-shaped record. #53 adds one small model, `EmailDelivery` (table `email_delivery`), owned by the email plugin and migrated the normal way (`plugin.migrations()`), written through the ORM (never raw SQL), one row per durable send:

| Column | Type | Meaning |
|---|---|---|
| `id` | PK | delivery id (PK-shape independent: i64 / String / Uuid) |
| `idempotency_key` | String | caller-supplied or derived key; a duplicate key is a no-op re-send (see below) |
| `to` | Json | the recipient list, for the operator's search |
| `subject` | String | denormalized for the admin list |
| `template` | Option<String> | the template name, when the body was rendered from one |
| `provider` | Option<String> | which backend/provider actually accepted it (`"smtp"`, `"resend"`, `"ses"`, ...) |
| `status` | String | `"queued" \| "sending" \| "sent" \| "failed" \| "dead_letter"` plus the deliverability states below |
| `provider_message_id` | Option<String> | the id the provider returned (the join key for bounce/complaint webhooks in #54) |
| `attempts` | i32 | mirrors the task/relay attempt count |
| `last_error` | Option<String> | the most recent `EmailError` display, redacted of secrets |
| `created_at` / `updated_at` | DateTime | audit timestamps |

`status` starts at `queued`, moves to `sending` when the handler claims it, and to `sent` when `send` returns `Ok`. `failed` is a retriable attempt that will be retried; `dead_letter` is the terminal give-up after `max_attempts`. The deliverability terminal states (`delivered`, `bounced`, `complained`) are stamped later by #54's inbound webhooks, keyed on `provider_message_id` - so a message that the relay handed off successfully but that the provider later hard-bounced does not read as `sent` forever.

### Idempotency keys

The queue and outbox both promise at-least-once, which means a crash after `send` succeeded but before the status was stamped re-runs the handler and sends the mail twice. Email has no consumer-side dedupe the way a webhook receiver does, so #53 adds the dedupe at the send boundary:

- Every durable send carries an `idempotency_key`. For `send_durable` the caller may pass one (`send_durable_with_key(&msg, "welcome:user:42")`); absent an explicit key, one is derived by hashing `(to, subject, body, template)` so an accidental double-enqueue of the identical message collapses. For the outbox path the key is the outbox event `id`, exactly the idempotency key the outbox already hands every destination.
- The `email_send` handler checks `EmailDelivery::objects().filter(email_delivery::IDEMPOTENCY_KEY.eq(&key)).first()` before sending. If a row already exists in a terminal `sent`/`delivered` state, the handler returns `Ok` without re-sending (the send already happened; the re-run is the at-least-once artifact). A unique index on `idempotency_key` makes the check a cheap point lookup and makes a concurrent double-claim fail closed at the database rather than double-sending.

This is honest about the guarantee: it is at-least-once delivery with best-effort de-duplication at the send boundary, not exactly-once (a provider that accepts the message but drops the connection before acking still forces a resend). We document it that way, matching the outbox doc's own at-least-once framing.

### Why this shape

- **It writes zero new retry infrastructure.** Backoff, the dead-letter ceiling, the claim discipline, the periodic relay, and the per-attempt log are all `umbral-tasks`' and `umbral-outbox`'s, already shipped. #53 adds one `#[task]`, one `Destination`, one model, and the `EmailError` -> retriable/permanent classification.
- **It honors what the plugin already promised.** The `Smtp` variant doc-comment and the module docstring both say durability arrives by wiring `send` through `umbral-tasks`; this is that wiring, named `send_durable`, plus the transactional variant the outbox makes possible.
- **`send` stays synchronous and un-retried for the caller who wants it.** The fire-and-forget path is unchanged; durability is an additive entry point, not a behavior change to `send`.

### Deferred / out of scope for #53

- Scheduled / delayed sends beyond what `EnqueueOptions::delay` / `scheduled_for` already give the queue.
- Rate-limiting sends per provider (a provider-quota concern; noted in #54's failover section, not built here).
- Exactly-once delivery (needs a consumer-side dedupe email does not have; we ship at-least-once + boundary de-dup).

---

## Part 2 (#54): broader providers, failover, and inbound deliverability

### What exists and what is missing

The API backend is hard-wired to two providers via the `EmailApiProvider` enum (`Resend`, `SendGrid`) and the pure `build_api_request` mapper. There is no SES, Postmark, or Mailgun; no failover from one provider to another; and no inbound path at all - a bounce or a spam complaint from the provider lands on a webhook URL nobody is listening on, so `EmailDelivery.status` can never advance past `sent`. #54 widens the outbound provider set behind the existing `Mailer` seam and adds the inbound models.

### More outbound providers behind the same seam

The extension point already exists and is deliberately shaped for this: `build_api_request(provider, key, msg, default_from) -> ApiRequest` is a pure, per-provider mapper, and `EmailApiProvider` is the closed enum it matches on. #54 widens the enum and adds one match arm per provider - no new dispatch machinery, the same `deliver_api` POST for all of them:

```rust,ignore
pub enum EmailApiProvider {
    Resend,       // existing
    SendGrid,     // existing
    Ses,          // new: POST https://email.<region>.amazonaws.com (SigV4-signed)
    Postmark,     // new: POST https://api.postmarkapi.com/email  (X-Postmark-Server-Token)
    Mailgun,      // new: POST https://api.mailgun.net/v3/<domain>/messages (basic auth "api:<key>")
}
```

Each provider is one arm in `build_api_request` producing its JSON (or form) body and endpoint, and `EmailApiProvider::from_setting` gains three case-insensitive strings (`"ses"`, `"postmark"`, `"mailgun"`). Two providers need a wrinkle beyond a plain bearer token, and both are handled inside the pure mapper plus a small addition to `ApiRequest`:

- **Postmark** authenticates with an `X-Postmark-Server-Token` header, not `Authorization: Bearer`. **Mailgun** uses HTTP basic auth (`api:<key>`) and an `application/x-www-form-urlencoded` body, and its endpoint embeds the sending domain. So `ApiRequest` grows from `{ url, bearer, body }` to carry an auth scheme and an optional content type (an additive change; the existing bearer path stays the default). The secret redaction in `ApiRequest`'s manual `Debug` extends to the new auth field so no key leaks into a log.
- **SES** needs AWS SigV4 request signing, which is more than a static header. The SES arm builds the canonical request and signs it with the account credentials from settings (`email_ses_region`, `email_ses_access_key`, `email_ses_secret_key`), reusing the same SigV4 signer `umbral-storage`'s S3 backend already depends on rather than pulling a second AWS crate.

Every new provider keeps the pure-mapper property: `build_api_request` does no I/O, so each provider's request shape is unit-testable without a network round-trip, exactly as the existing Resend/SendGrid tests already assert.

### Provider failover

Failover is a policy over an ordered provider list, and it rides the queue's retry, not a second mechanism. `EmailPlugin` gains a builder:

```rust,ignore
EmailPlugin::new()
    .provider(EmailApiProvider::Ses)           // primary
    .failover_to(EmailApiProvider::Postmark)    // on retriable failure, next attempt uses this
    .failover_to(EmailApiProvider::Mailgun)
```

The durable `email_send` handler (from #53) uses the attempt number to pick the provider: attempt 1 uses the primary, and each retriable failure that pushes the send to its next attempt advances to the next provider in the list (wrapping or stopping at the last, per config). Because provider selection is a function of the queue's existing `attempts` counter, failover costs nothing beyond the ordered list and one index computation - the backoff, the ceiling, and the delivery log are the queue's, unchanged. A permanent error (bad payload) still dead-letters immediately regardless of remaining providers; failover only applies to the retriable set, since a malformed message fails identically on every provider.

Failover is only meaningful on the durable path (the queue is what carries the attempt count). Synchronous `send` keeps its single configured backend; an app that wants failover uses `send_durable`.

### Inbound bounce and complaint webhooks

The providers report hard bounces, soft bounces, and spam complaints by POSTing to a webhook URL. #54 adds the inbound receiver as routes the email plugin contributes, plus two models, so `EmailDelivery` learns the final outcome and a suppression list builds itself.

**Verifying the inbound POST.** Each provider signs its webhook differently (SES/SNS signs with a certificate; Postmark and Mailgun use a shared secret or HMAC). The receiver validates the provider's signature before trusting the body, reusing `umbral-webhooks`' receiver-side `verify_signature` helper where the scheme matches (HMAC providers) and a provider-specific verifier where it does not (SNS certificate validation). An unverified POST is rejected with 401 before any row is written - the same fail-closed posture as the rest of the framework.

**Model 1: the inbound event log (`EmailEvent`).** One row per provider callback, written through the ORM:

| Column | Type | Meaning |
|---|---|---|
| `id` | PK | event id |
| `provider` | String | which provider sent the callback |
| `provider_message_id` | String | joins back to `EmailDelivery.provider_message_id` |
| `event_type` | String | `"delivered" \| "bounce" \| "complaint" \| "open" \| "click"` (normalized across providers) |
| `bounce_type` | Option<String> | `"hard" \| "soft"` for a bounce |
| `recipient` | String | the address the event is about |
| `raw` | Json | the provider's original payload, for debugging |
| `received_at` | DateTime | callback timestamp |

On a `delivered`/`bounce`/`complaint` event, the receiver updates the matching `EmailDelivery` row's `status` (to `delivered`, `bounced`, or `complained`), closing the loop #53 opened - a delivery is not "done" when the relay hands it off, it is done when the provider confirms it landed or bounced.

**Model 2: the suppression list (`EmailSuppression`).** A hard bounce or a complaint writes a suppression row so the app stops mailing an address that has already bounced or complained (mailing a known-bad address repeatedly is what gets a sending domain blocklisted):

| Column | Type | Meaning |
|---|---|---|
| `id` | PK | suppression id |
| `address` | String | the suppressed recipient (unique index) |
| `reason` | String | `"hard_bounce" \| "complaint" \| "manual" \| "unsubscribe"` |
| `source_event_id` | Option<FK -> EmailEvent> | the event that caused it (NULL for manual/unsubscribe) |
| `created_at` | DateTime | when suppressed |

The `email_send` handler consults the suppression list before sending: a recipient with a `hard_bounce` or `complaint` suppression is skipped (the send resolves to `dead_letter` with a clear "recipient suppressed" reason, not a silent drop), so suppression is enforced at the send boundary the same place idempotency is. Manual suppressions (an operator adds one) and unsubscribes (from #55's preference layer) write into the same table, so all "do not mail this address" reasons flow through one gate.

### Unsubscribe groups

Transactional mail (a password reset) must always send; marketing / digest mail (a weekly summary) must honor an opt-out. An `EmailSuppression` with reason `unsubscribe` is address-wide and too blunt for that split, so #54 adds unsubscribe *groups*: named categories a recipient can opt out of independently.

- A small `UnsubscribeGroup` model (name, description, whether it is transactional-and-therefore-not-suppressible) plus a per-recipient `UnsubscribeGroupMembership` (address, group, opted_out_at).
- A send carries an optional group (`send_durable_grouped(&msg, "weekly_digest")`). The handler checks group membership for the recipient before sending; an opted-out recipient in a non-transactional group is skipped. A transactional group can never be unsubscribed from, so a password reset is never suppressed by an over-eager global unsubscribe.
- The framework generates the per-recipient unsubscribe link and the `List-Unsubscribe` header (the one-click header modern inbox providers increasingly require for bulk mail), pointing at a route the email plugin contributes that records the opt-out into `UnsubscribeGroupMembership`. This is the same header-and-link plumbing #55's digest channel will lean on.

### Why this shape

- **It widens the existing seam, not a new one.** `build_api_request` was already a pure per-provider mapper over a closed enum; adding SES/Postmark/Mailgun is three match arms plus one additive `ApiRequest` field for the non-bearer auth schemes. The dispatch, the `deliver_api` POST, and the config resolution are unchanged.
- **Failover is free of new machinery.** It is a function of the queue's `attempts` counter over an ordered provider list; the retry, backoff, and log are #53's/`umbral-tasks`'.
- **Inbound closes the delivery-status loop honestly.** `EmailDelivery.status` reaches a true terminal (`delivered`/`bounced`/`complained`) only when the provider confirms it, and a hard bounce feeds a suppression list that protects the sending reputation - reusing `umbral-webhooks`' verification for the receiver rather than re-implementing signature checks.

### Deferred / out of scope for #54

- Full deliverability analytics / reputation dashboards (a projection over `EmailEvent`; the raw material is here, the dashboard is an admin custom view later).
- DKIM / SPF / DMARC key management (a DNS + relay concern; the plugin still delegates signing to the relay/provider, as the module docstring already states).
- Inbound email *parsing* (receiving replies, not just bounce callbacks) - a separate inbound-mail feature, noted not designed.

---

## Part 3 (#55): unified notifications (`umbral-notifications`)

### What #55 asks for, against what exists

gaps5 #55: umbral has email, tasks, and realtime as separate primitives; Laravel has notification channels and Firebase has messaging, both a single "notify this user, across the channels they prefer" abstraction. #55 adds `plugins/umbral-notifications`, a plugin that sits over the durable queue (#53), the widened email (#54), the realtime plugin, and the webhook destination, and gives one `Notification` abstraction that fans out to whichever channels a recipient prefers, with templates, preferences, digests, and delivery logs.

This is deliberately a layer, not a new transport. Every channel it fans out to is a mechanism that already exists or is designed in this doc-set; `umbral-notifications` is the router and the preference/template/digest logic on top.

### The `Notification` abstraction

A notification is defined once and rendered per channel. The core trait:

```rust,ignore
#[async_trait]
pub trait Notification: Send + Sync {
    /// Stable type name, e.g. "order.shipped" - the preference + digest key.
    fn kind(&self) -> &'static str;

    /// Which channels this notification wants, before per-user preferences filter them.
    fn channels(&self) -> &[ChannelKind];

    /// Render the per-channel payload. The notification owns its content;
    /// the channel owns the transport.
    fn render(&self, channel: ChannelKind, recipient: &Recipient) -> ChannelPayload;
}
```

`ChannelKind` is the closed set of built-in channels: `Email`, `Sms`, `Push`, `Slack`, `Webhook`, `InApp`. Delivering a notification is `notifications::notify(recipient, &notification).await?`, which:

1. Resolves the recipient's per-user preferences for `notification.kind()` (below), producing the effective channel set (the intersection of what the notification wants and what the user has not disabled).
2. For each effective channel, renders the payload and enqueues a durable delivery on `umbral-tasks` (one task per channel, so a Slack outage does not block the email). Each channel delivery gets the same retry/backoff/dead-letter treatment #53 established, because it rides the same queue.
3. Writes a `NotificationDelivery` log row per (notification, recipient, channel).

### The channels, each over an existing mechanism

| Channel | Delivers via | Notes |
|---|---|---|
| `Email` | `umbral-email` `send_durable` (#53) | honors suppression + unsubscribe groups (#54); marketing notifications map to a non-transactional group |
| `Sms` | a `SmsProvider` trait (Twilio / an app-supplied impl) | new thin transport, same pure-mapper + `reqwest` POST shape as the email API backend; ships one Twilio adapter |
| `Push` | a `PushProvider` trait (APNs / FCM) | new thin transport; token storage is a per-recipient `PushToken` model |
| `Slack` | the `webhook` `Destination` posting a Slack incoming-webhook payload | reuses `umbral-webhooks` delivery + retry; a Slack channel is a webhook URL |
| `Webhook` | the `webhook` `Destination` (`umbral-webhooks`) | a notification fanned out as a signed webhook to a registered endpoint |
| `InApp` | a `Notification` row in the app's DB + `umbral-realtime` push | the durable in-app inbox; realtime delivers the live frame, the row is the replayable record |

The two genuinely new transports are `Sms` and `Push`. Both follow the email API backend's proven shape exactly: a pure mapper from the rendered payload to a provider request (unit-testable without a network call), a `reqwest` POST behind a cargo feature, and a closed provider enum widened per adapter. `Slack`, `Webhook`, and `InApp` are pure composition over `umbral-webhooks` and `umbral-realtime` - no new transport code. This keeps #55 honest about its size: two new transports, one router, and the preference/template/digest logic; everything else is plumbing that already exists.

### Templates

A notification renders per channel, and each channel wants a different shape (an email has subject + HTML + text; an SMS is one short string; a push has title + body; an in-app has a title + link). The template convention extends `umbral-email`'s existing one (`email/<name>.txt`, `email/<name>.html` rendered via `umbral::templates::render`):

```
notifications/<kind>/email.html
notifications/<kind>/email.txt
notifications/<kind>/sms.txt
notifications/<kind>/push.json
notifications/<kind>/slack.json
notifications/<kind>/in_app.html
```

`Notification::render` defaults to loading the channel's template by convention and rendering it with the notification's context, so a simple notification is a struct plus a folder of templates with no `render` body written by hand. A notification that needs code-built payloads overrides `render`. This reuses the framework template engine (`render_email_body` is the existing thin wrapper); no second templating system.

### Per-user preferences

One model, `NotificationPreference`, one row per (recipient, notification kind, channel), storing whether that channel is enabled for that kind:

| Column | Type | Meaning |
|---|---|---|
| `recipient` | String | the user PK string (PK-shape independent) |
| `kind` | String | the notification kind, or `"*"` for a default |
| `channel` | String | the `ChannelKind` |
| `enabled` | bool | opt-in / opt-out for this (kind, channel) |
| `digest` | Option<String> | if set, batch this channel into a digest window (`"daily" \| "weekly"`) instead of sending immediately |

Resolution is most-specific-wins: an exact `(kind, channel)` row beats a `("*", channel)` default beats the notification's own declared channel set. A `notify` call intersects the notification's requested channels with the user's enabled channels, so a user who has turned off SMS for `order.shipped` simply never gets the SMS task enqueued. Certain kinds are markable transactional-and-non-suppressible (a security alert), mirroring #54's transactional-group rule, so a critical notification cannot be silenced into nothing.

The preferences surface is editable through the admin (a custom view) and through a self-serve route the plugin contributes (the "notification settings" page a user expects), both writing the same model through the ORM.

### Digests

A preference row with `digest = "daily"` for a channel diverts that channel's deliveries into a batch instead of sending each immediately. The digest is built on the queue's periodic beat, not a new scheduler:

- Instead of enqueuing an immediate channel task, `notify` appends a `PendingDigestItem` (recipient, channel, kind, rendered payload, window). This is a durable row, so a crash does not lose a queued digest item.
- A `#[task] periodic` digest builder runs per window (daily / weekly, on the `TasksPlugin::periodic` beat), collects each recipient's pending items for the window, renders them into one combined notification (via a `notifications/<...>/digest.html` template), enqueues a single durable channel send, and clears the collected items.
- This reuses the exact periodic + durable-task pattern the outbox relay and #53 already use; the digest builder is one more periodic task, not a second scheduling system.

### Delivery logs

One model, `NotificationDelivery`, one row per (notification, recipient, channel) attempt, giving the audit trail #55 asks for:

| Column | Type | Meaning |
|---|---|---|
| `id` | PK | delivery id |
| `notification_kind` | String | which notification |
| `recipient` | String | the target user |
| `channel` | String | which channel |
| `status` | String | `"queued" \| "sent" \| "delivered" \| "failed" \| "dead_letter" \| "suppressed" \| "digested"` |
| `provider_ref` | Option<String> | the underlying transport's id (the `EmailDelivery.id`, the SMS provider message id, the webhook delivery id) |
| `error` | Option<String> | last error, secret-redacted |
| `created_at` / `updated_at` | DateTime | audit timestamps |

The `provider_ref` links each notification delivery to the underlying transport's own record (`EmailDelivery`, `WebhookDelivery`, a push/SMS provider id), so an operator can trace a notification from "user should have been told" down to the concrete provider attempt, across channels, in one join. The log is surfaced in the admin as a filterable table (by kind, recipient, channel, status), charts via ApexCharts and icons via Lucide per the standardized-libraries convention.

### Why this shape

- **It is a router over shipped mechanisms, not a new transport stack.** Email is #53/#54; Slack and Webhook are `umbral-webhooks`; in-app is `umbral-realtime` plus a durable row; the queue, backoff, dead-letter, and periodic beat are `umbral-tasks`. The only genuinely new code is the `Sms` and `Push` transports (each the email API backend's pure-mapper shape) and the preference/template/digest/log logic.
- **Every fan-out is durable by construction.** Each channel delivery is a `umbral-tasks` task, so a notification inherits the same at-least-once retry and dead-letter semantics email got in #53; a single channel failing does not lose the others.
- **Preferences and digests reuse existing seams.** Preferences are one model resolved most-specific-wins; digests are one periodic task on the existing beat; templates extend the existing `umbral::templates` convention. No second scheduler, no second template engine, no second queue.
- **`umbral-core` stays plugin-free.** `umbral-notifications` is a plugin depending on the `umbral` facade plus sibling plugins (`umbral-email`, `umbral-tasks`, `umbral-webhooks`, `umbral-realtime`); core never learns notifications exist. If any channel could not be a plugin, the plugin contract would be wrong.

### Deferred / out of scope for #55

- Rich in-app notification center UI beyond the durable inbox rows + realtime push (a frontend product on top of the models).
- Provider adapters beyond one each for SMS (Twilio) and push (APNs/FCM); more adapters are additional enum arms, the same widening #54 does for email providers.
- Cross-channel deduplication ("the user already saw this in-app, skip the email") - a policy refinement over the delivery log, noted not designed.
- Localization / per-recipient locale template selection (the template convention leaves room for a `<kind>/<locale>/` layer; not built here).

---

## Summary of the contract

- **#53:** email gains a durable path without changing `send`. `send_durable(&msg)` enqueues a `#[task] email_send` that wraps the existing `send`, with `EmailError` classified into retriable (SMTP/transport/5xx/429) and permanent (config/data/4xx) so the queue's backoff and dead-letter ceiling apply correctly; `outbox::publish_on(tx, ..)` gives the transactional variant (mail commits atomically with the business row) via an `email` outbox `Destination`. An `EmailDelivery` model records status, provider, `provider_message_id`, attempts, and an idempotency key that de-duplicates the at-least-once resend at the send boundary. No new retry loop or scheduler; it composes `umbral-tasks` and `umbral-outbox`.
- **#54:** the `EmailApiProvider` enum and the pure `build_api_request` mapper widen with SES (SigV4, reusing the storage plugin's signer), Postmark (token header), and Mailgun (basic auth + form body), one match arm each, with `ApiRequest` gaining an additive auth field. Provider failover is a function of the queue's `attempts` over an ordered `.failover_to(..)` list, no new machinery. Inbound bounce/complaint webhooks (verified via `umbral-webhooks`' `verify_signature` / a provider verifier) write an `EmailEvent` log, advance `EmailDelivery.status` to a true terminal (`delivered`/`bounced`/`complained`), and feed an `EmailSuppression` list enforced at the send boundary, plus `UnsubscribeGroup`s with a `List-Unsubscribe` header so transactional mail always sends and bulk mail honors opt-out.
- **#55:** a `umbral-notifications` plugin adds a `Notification` trait (`kind`, `channels`, `render`) that fans out to `Email`/`Sms`/`Push`/`Slack`/`Webhook`/`InApp`, each channel delivered as a durable `umbral-tasks` task over an existing mechanism (email is #53/#54; Slack/Webhook are `umbral-webhooks`; in-app is `umbral-realtime` + a row; only SMS and push are new transports, each the email API backend's pure-mapper shape). Per-user `NotificationPreference` resolves most-specific-wins to filter channels, digests batch a channel's items on the existing periodic beat, templates extend the `umbral::templates` convention per channel, and a `NotificationDelivery` log links each fan-out back to its underlying transport record. It is a router over shipped mechanisms; `umbral-core` never learns it exists.
