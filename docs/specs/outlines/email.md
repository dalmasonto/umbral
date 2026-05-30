# Outline — Email

| | |
|---|---|
| **Status** | Outline. Promotes when password reset or any built-in needs email. |
| **Maps to milestone** | M9–M11 |
| **Companions** | `01-app-and-settings.md`, `02-plugin-contract.md`, outlines `templates.md`, `tasks.md`, `auth-and-sessions.md`, `static-and-media.md` |

## Purpose

`umbra::mail` is the framework's outbound-email pipeline: a Django-shape `send_mail(...)` helper for the simple case, a structured `EmailMessage` for everything else, and a pluggable `EmailBackend` so dev, test, and prod each get the right delivery semantics without changing call sites. It is core utility rather than an optional plugin — the moment password reset lands in `umbra-auth` (cross-link: outline `auth-and-sessions.md`), the framework needs a guaranteed way to deliver a token to a user's inbox, and a "maybe install this crate" story would let security-critical flows silently no-op. Backends matter because the right behaviour is environment-specific: dev wants to *see* the mail (console), tests want to *assert* on it without sending (dummy capture), staging wants to *review* it offline (file), and prod wants real SMTP. One module, four backends, one accessor — the same shape the DB pool and task queue from `01-app-and-settings.md` already use.

## Key concepts

**`send_mail` — the simple case.** A one-line helper for the 80% case: subject, body, from, to. Internally it builds an `EmailMessage` and hands it to the active backend, so there is one delivery path, not two.

```rust
umbra::mail::send_mail(
    "Welcome to umbra",
    "Hi there — thanks for signing up.",
    "noreply@example.com",
    &["alice@example.com"],
).await?;
```

**`EmailMessage` — the structured form.** Everything `send_mail` hides: cc, bcc, reply-to, custom headers, attachments, and an HTML alternative part. Built through a builder so optional fields stay ergonomic.

```rust
let msg = EmailMessage::builder()
    .subject("Reset your password")
    .from("noreply@example.com")
    .to("alice@example.com")
    .body_text(text_body)
    .body_html(html_body)
    .build()?;
umbra::mail::send(msg).await?;
```

**Templated emails.** Bodies render through the templates engine (cross-link: outline `templates.md`); a plugin ships `templates/email/<name>.txt.j2` and `templates/email/<name>.html.j2` as a pair, and `umbra::mail::render_and_send("auth/password_reset", &ctx, recipients)` is the convention. The `.txt` version is the canonical body; the `.html` version becomes the multipart alternative if present.

**Backends.** Four ship in-box, all behind one `EmailBackend` trait so swapping is a settings change.
- **SMTP** — production delivery. Crate choice is open (see below); the surface is `SmtpBackend::new(host, port, credentials)` and a Postgres-style connection-pool config.
- **Console** — prints the rendered message to stderr. The dev default, so a local password-reset visibly shows up in the terminal.
- **File** — appends each message to a directory (one `.eml` per send). Useful on staging or shared dev boxes where a developer wants to review what was sent without running a real SMTP server.
- **Dummy** — accepts and drops, recording the message in an in-process buffer for tests to assert against. The test default.

**Backend selection.** `Settings.email_backend` picks one (`Smtp | Console | File { dir } | Dummy`); per-environment overrides ride the same `UMBRA_EMAIL_BACKEND=...` env var path every other setting uses. The system check from `05-backends-and-system-check.md` warns if `Console` or `Dummy` is selected with `environment = Prod`.

**Ambient handle.** Set during `App::build()` into the `umbra::mail` `OnceLock`, matching the pattern in `01-app-and-settings.md`. `umbra::mail::send(...)` reads it; no `State<EmailBackend>` threads through handler signatures.

**Background sending.** Synchronous SMTP from a request handler is the wrong default — a slow relay stalls the response. `umbra::mail::enqueue(msg)` (cross-link: outline `tasks.md`) is a thin wrapper that serializes the `EmailMessage` and submits it to the task queue as a built-in `send_email_task`; the worker runs the actual `EmailBackend::send`. Password reset uses this path.

## Promote-to-deep trigger

Promote at M9 entry, the moment `umbra-auth`'s password-reset flow needs to deliver a token — that's the forcing function that turns the four backends and the templated-message contract from a sketch into shipped code.

## Open questions

- **SMTP crate choice.** `lettre` is mature, widely used, and supports STARTTLS / OAuth2 / connection pooling out of the box; `mail-send` is lighter and async-first. Settle by benchmarking the worker-loop send path against a real relay at M9.
- **Attachment representation.** Three options: a filesystem path (cheap, but couples to local disk), in-memory `Bytes` (works everywhere, ugly for large files), or a `Storage` handle from outline `static-and-media.md` (clean cross-link, requires that outline to land first). The right answer probably blends path-or-bytes for now and adds the `Storage` variant once media storage is concrete.
- **HTML vs text alternative parts.** Convention is "ship both, let the client pick"; open is whether the templates pair is *required* (system-check warns on `.html.j2` without `.txt.j2`) or merely conventional.
- **DKIM / SPF / DMARC integration.** Likely "use your SMTP relay" — umbra signs nothing itself in the first iteration. Revisit if a credible self-signing story surfaces; until then, the docs point users at SES / Postmark / Mailgun / a self-hosted Postfix relay.
- **Inbound email.** Out of scope. Django doesn't ship inbound either; if a real ask appears, it lands as its own outline.

## Cross-links

- Deep specs that constrain this: `01-app-and-settings.md` (the `umbra::mail` `OnceLock`, `Settings.email_backend`, environment-aware override), `02-plugin-contract.md` (plugins can ship `templates/email/` directories and register defaults during `on_ready`).
- Sibling outlines: `templates.md` (rendering email bodies, the `.txt` / `.html` pair convention), `tasks.md` (`enqueue_email` is a wrapper over the task queue), `auth-and-sessions.md` (password reset is *the* driver that promotes this outline), `static-and-media.md` (attachment storage handle, once available).
