# umbra-email — holistic review

Read-only review, 2026-06-16. Scope: `plugins/umbra-email/src/lib.rs` + `tests/`. Cross-referenced against `planning/hardening/backlog.md`, `reviews/security.md`. The backlog's docs-audit said "`umbra-email` referenced as shipped (crate doesn't exist)" — **that is now stale; the crate exists, builds, and is a workspace member.** Findings already filed are tagged **(already #N)**; everything else is **NEW**.

## Verdict

**Complete and honest for a v1 transactional-email plugin: SMTP (STARTTLS) + console backend + template rendering + attachments + text/HTML alternatives, all real and tested.** Scope boundaries (no CC/BCC, no queue, no file backend) are *explicitly documented in the crate docs as v1 deferrals*, not silent stubs — which is exactly the right way to ship a thin slice. The one operational sharp edge (console backend printing full message bodies, incl. reset tokens, to stderr outside Dev) is *warned about loudly* rather than hidden. No `todo!()`, no swallowed errors, no broken paths.

Completeness one-liner: **Core `send`/`compose`/templates/attachments are complete and tested; CC/BCC, a file backend, and queued/retried send (via umbra-tasks) are documented deferrals, not bugs.**

## Completeness

| Capability | State | Note |
|---|---|---|
| SMTP backend | **Complete** | `lettre` STARTTLS relay on port 587, optional SASL creds, rustls. |
| Console backend (dev default) | Complete | prints rendered RFC822 to stderr; auto-selected when `email_smtp_host` unset or `UMBRA_EMAIL_BACKEND=console`. |
| File backend | **Absent** | Django has `filebased.EmailBackend`. Not shipped; not documented as deferred either → small doc gap. |
| Dummy / locmem backend | Absent | tests use console; no in-memory capture backend for assertions. |
| `send` API | Complete | resolves From (message → `email_default_from` → error), validates recipients. |
| `EmailMessage` builder | Complete | from/to/add_to/subject/text_body/html_body/reply_to/attach. Django `EmailMessage` parity for the common path. |
| HTML + text alternatives | Complete | `multipart/alternative`, tested. |
| Attachments | Complete | bytes-only, `multipart/mixed`, base64 by lettre, invalid-content-type → named error. Tested thoroughly (`tests/attachments.rs`). |
| Templated emails | Complete | `render_email_body` → `umbra::templates::render`, tested against a real template file. |
| **CC / BCC** | **Absent** | documented v1 deferral. |
| **Queued / retried send** (umbra-tasks) | **Absent** | documented v1 deferral; transient SMTP errors bubble as `EmailError::Smtp`. |
| Inline images (`cid:`) | Absent | documented deferral. |
| DKIM / S-MIME | Absent | documented deferral (use relay). |
| Stubs / `todo!()` / no-ops | **None** | `ConsoleBackend::deliver` returns `Result` only for parity, documented; not a no-op. |

## Findings

### NEW — Important (security: email header injection)
- **No CRLF / header-injection guard on `subject` (or `reply_to`/`to` when caller-supplied).** `lib.rs:474` passes `message.subject` straight to `Message::builder().subject(...)`, and recipients/from parse through `Mailbox`. lettre's typed builder *does* reject control chars in addresses (the `Mailbox` parse) and encodes the subject, **so in practice lettre is the guard** — but this plugin never states or tests that contract, and a subject containing `\r\nBcc: attacker@evil` is the classic injection that a templated subject built from user input (e.g. `"Re: {user_supplied}"`) would carry. Verify lettre's subject encoding actually neutralises embedded CRLF (it should, via RFC 2047 encoded-words) and **add a test pinning it** so a future lettre swap can't regress it. If unconfirmed, sanitise subject (strip `\r`/`\n`) before handing to the builder. → **NEW gap** (verify-and-test; likely already-safe via lettre but unproven here).

### NEW — Important (operational, already self-flagged)
- **Console backend prints full message bodies — including password-reset tokens / magic links — to stderr, and outside Dev only *warns*.** `lib.rs:434-457`. The code already names this (`gaps: BROKEN-6` comment) and warns loudly when not in Dev, which is the right call, but a misconfigured prod (missing `email_smtp_host`) will *still send every reset token to the log aggregator* while merely warning. This is a real prod footgun even with the warning. Consider: in `Environment::Prod` with no SMTP host, **fail the send** (`EmailError`) rather than fall through to stderr, so a missing relay config surfaces as a hard error at the call site instead of a silent token leak. → **NEW gap** (or confirm the warn-only posture is the deliberate decision and log it).

### NEW — Optional (correctness / availability)
- **SMTP send has no timeout.** `lib.rs:557-575` builds the transport with no connection/operation timeout, so a black-holed relay hangs the awaiting task indefinitely. Fix: `.timeout(Some(Duration::from_secs(...)))` on the `AsyncSmtpTransport` builder. → **NEW gap.**
- **`EmailConfig` is cached in a process-lifetime `OnceLock` (`lib.rs:344-348`)** — consistent with the framework's ambient-handle convention and documented, but means a settings reload needs a restart. FYI, not a defect.
- **No backend abstraction trait.** The backend is a `BackendKind` enum dispatched in `send` (`lib.rs:433`). A third-party can't add (say) an SES-API backend without forking. Django exposes a pluggable `EmailBackend`. Fine for v1; note for the future. FYI.

### Already filed (cross-ref)
- The docs-audit "`umbra-email` crate doesn't exist" entry in `backlog.md` P1 docs long-tail is **stale** — the crate ships. The doc page (`documentation/docs/v0.0.1/plugins/email.mdx`) exists. → flag for the doc-fix batch to drop that line.

## Architecture / plugin-contract

Clean. Facade-only (`use umbra::prelude::*`, `umbra::templates`, `umbra::Settings`) — no core internals, no other-plugin deps. Service-shaped plugin: no models, no migrations, no routes (correct — it's a send service). `Plugin` impl is minimal and honest (just `name()`). No raw `sqlx` (touches no DB). The one architectural observation (above): `BackendKind` enum vs a `Backend` trait — acceptable for the documented v1 scope. Error enum is thorough with `From` impls so `?` flows. `compose` is public so a future `umbra-tasks` queue path can serialise a `lettre::Message` — good forward-design.

## Tests

Solid, behavioral, real. `tests/integration.rs` boots a real `App` with `EmailPlugin` + a real template file and exercises console send, template rendering (substituted values asserted), and both error variants (NoRecipients, MissingFrom). `tests/attachments.rs` drives the public `compose` and inspects actual RFC822/MIME wire bytes — content-type, filename, base64 payload, multipart boundaries, nested alternative-in-mixed, multiple attachments, invalid-content-type error. This is exactly the "round-trip the real artifact" style the repo asks for.

**Gaps:**
- **No SMTP-path test** — understandable (needs a relay), but a `lettre` stub transport or a captured-message backend would let the From-resolution + transport-build logic be asserted without a socket. The `deliver_smtp` host-`.expect` and creds-conditional branch are untested.
- **No header-injection test** (the subject-CRLF concern above) — the single most valuable security test to add.
- **No test that the console-backend prod warning fires** — the BROKEN-6 path is untested.
- **No HTML-only-no-text and no-bodies-at-all `send` tests** at the `send` level (only at `compose` level via attachments).
