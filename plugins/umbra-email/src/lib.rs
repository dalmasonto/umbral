//! umbra-email. SMTP + template-driven transactional email.
//!
//! Django's `django.core.mail` shape. The 80% case is a single
//! `send(&EmailMessage)` call against an ambient SMTP transport
//! configured from `umbra::Settings`. The dev default is a stderr
//! "console" backend that prints the rendered message instead of
//! talking to a relay. A fresh `cargo run` exercises password reset
//! and welcome flows without anyone wiring SMTP credentials.
//!
//! ## Settings keys
//!
//! All read from `umbra::Settings::extra` (the catch-all
//! `UMBRA_<KEY>` / `umbra.toml` keys). Defaults in parentheses.
//!
//! - `email_smtp_host`. Relay hostname. Absent means console backend.
//! - `email_smtp_port`. Relay port (587, STARTTLS).
//! - `email_smtp_user`. SASL username. Optional.
//! - `email_smtp_password`. SASL password. Optional.
//! - `email_default_from`. Fallback sender when `EmailMessage.from`
//!   is empty.
//! - `email_api_provider`. `"resend"` or `"sendgrid"`. Selects the HTTP
//!   API backend (requires the `api` cargo feature).
//! - `email_api_key`. Bearer token for the API provider.
//!
//! ## Backend selection
//!
//! `UMBRA_EMAIL_BACKEND=console` forces the console backend even when
//! other keys are present (useful in CI / tests). Otherwise the order
//! is: `UMBRA_EMAIL_BACKEND=api` (or both `email_api_provider` +
//! `email_api_key` set) ⇒ **API**; else `email_smtp_host` set ⇒
//! **SMTP**; else **console**. The API backend POSTs JSON to a
//! transactional-email provider (Resend / SendGrid) and complements —
//! does not replace — SMTP.
//!
//! ## Surface
//!
//! - [`EmailMessage`]. Builder-shaped message struct.
//! - [`send`]. Push a message through the configured backend.
//! - [`render_email_body`]. Thin wrapper over
//!   [`umbra::templates::render`] mapping its error into [`EmailError`].
//! - [`ConsoleBackend`]. The dev fallback. Prints to stderr.
//! - [`EmailPlugin`]. Registers the plugin under the name `"email"`.
//!
//! ## v1 scope
//!
//! - No retry queue. Transient SMTP failures bubble up as
//!   `EmailError::Smtp`. Wiring this through `umbra-tasks` lands in a
//!   future round (`enqueue("send_email", payload)`).
//! - File attachments shipped via [`EmailMessage::attach`]. Inline
//!   images (`cid:` references from HTML) and CC / BCC are still
//!   future work — the gap a real consumer surfaces first wins.
//! - No S/MIME or DKIM signing. Use your relay's signing.
//! - One recipient list (`to`). Multiple recipients work; CC and BCC
//!   do not.
//! - The console backend is the default when `email_smtp_host` is
//!   absent. The alternative (silently no-op) was rejected: a missing
//!   env var that drops a password reset on the floor is the kind of
//!   production footgun this plugin exists to avoid.

#![allow(clippy::result_large_err)]

use std::sync::OnceLock;

use lettre::message::header::{ContentDisposition, ContentType};
use lettre::message::{Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use serde::Serialize;
use umbra::prelude::*;
use umbra::templates::TemplateError;

/// Default sender used when an `EmailMessage` has no explicit `from`
/// and no `email_default_from` setting is configured. Matches Django's
/// `DEFAULT_FROM_EMAIL` shape. A real deployment overrides it.
pub const FALLBACK_FROM: &str = "webmaster@localhost";

// =========================================================================
// Plugin
// =========================================================================

/// The email plugin. Service-shaped: contributes no models, no
/// routes, no system checks. Registering it is what wires the SMTP
/// transport into the App.
#[derive(Debug, Default)]
pub struct EmailPlugin;

impl Plugin for EmailPlugin {
    fn name(&self) -> &'static str {
        "email"
    }
}

// =========================================================================
// Message
// =========================================================================

/// A composed email message. Build with [`EmailMessage::new`] or via
/// the chained setters; pass to [`send`].
///
/// `from` and `to` are required (the latter must be non-empty). When
/// `from` is the empty string, [`send`] falls back to the
/// `email_default_from` setting, then to [`FALLBACK_FROM`].
///
/// `text_body` and `html_body` are independent: ship neither (an empty
/// body), either, or both (sent as `multipart/alternative` so the
/// client picks).
#[derive(Debug, Clone, Default)]
pub struct EmailMessage {
    pub from: String,
    pub to: Vec<String>,
    pub subject: String,
    pub text_body: Option<String>,
    pub html_body: Option<String>,
    pub reply_to: Option<String>,
    /// File attachments. Each lands as a separate part under a
    /// `multipart/mixed` envelope when the message is composed.
    /// Order is preserved; empty by default. See [`Attachment`] and
    /// [`Self::attach`].
    pub attachments: Vec<Attachment>,
}

/// One file attachment carried by an [`EmailMessage`].
///
/// The bytes-only shape is intentional for v1: no path-loading
/// (`std::fs::read` is one line for the file case), no auto content-
/// type detection (the caller knows what they generated), no inline-
/// image / `cid:` support (use a hosted CDN URL in the HTML body
/// instead). When a real consumer surfaces a need for any of those,
/// the API extends — adding fields is non-breaking, taking them away
/// later isn't.
///
/// Construct via [`Self::new`] or use [`EmailMessage::attach`] to
/// register one in a builder chain without naming the struct.
#[derive(Debug, Clone)]
pub struct Attachment {
    /// Filename surfaced to the recipient (the `filename=` parameter
    /// in the `Content-Disposition` header). Sanitised by lettre at
    /// header-render time — no escaping needed at the call site.
    pub filename: String,
    /// MIME content type. Use the canonical `type/subtype` form
    /// (e.g. `"application/pdf"`, `"image/png"`). Invalid content
    /// types surface as a `lettre` error during `send`.
    pub content_type: String,
    /// Raw bytes. Gets base64-encoded by lettre at MIME-render time;
    /// pass the unencoded payload here.
    pub data: Vec<u8>,
}

impl Attachment {
    /// Build an attachment from its three required pieces.
    pub fn new<F: Into<String>, C: Into<String>>(
        filename: F,
        content_type: C,
        data: Vec<u8>,
    ) -> Self {
        Self {
            filename: filename.into(),
            content_type: content_type.into(),
            data,
        }
    }
}

impl EmailMessage {
    /// Start a new message with subject and recipients.
    pub fn new<S: Into<String>>(subject: S, to: Vec<String>) -> Self {
        Self {
            subject: subject.into(),
            to,
            ..Self::default()
        }
    }

    /// Set the From header.
    pub fn from<S: Into<String>>(mut self, from: S) -> Self {
        self.from = from.into();
        self
    }

    /// Replace the recipient list.
    pub fn to(mut self, to: Vec<String>) -> Self {
        self.to = to;
        self
    }

    /// Append a single recipient.
    pub fn add_to<S: Into<String>>(mut self, to: S) -> Self {
        self.to.push(to.into());
        self
    }

    /// Set the subject.
    pub fn subject<S: Into<String>>(mut self, subject: S) -> Self {
        self.subject = subject.into();
        self
    }

    /// Set the plain-text body. Pair with [`html_body`] for a
    /// `multipart/alternative` send.
    pub fn text_body<S: Into<String>>(mut self, body: S) -> Self {
        self.text_body = Some(body.into());
        self
    }

    /// Set the HTML body. Pair with [`text_body`] for a
    /// `multipart/alternative` send.
    pub fn html_body<S: Into<String>>(mut self, body: S) -> Self {
        self.html_body = Some(body.into());
        self
    }

    /// Set the Reply-To header.
    pub fn reply_to<S: Into<String>>(mut self, reply_to: S) -> Self {
        self.reply_to = Some(reply_to.into());
        self
    }

    /// Add a file attachment. The message is composed as
    /// `multipart/mixed` when any attachments are present; the body
    /// (text / html / both) lands as one part and each attachment
    /// follows.
    ///
    /// Pass the raw bytes — lettre base64-encodes them at the
    /// MIME-render step. For a file on disk, read it yourself:
    ///
    /// ```ignore
    /// let pdf = std::fs::read("invoice.pdf")?;
    /// let msg = EmailMessage::new("Your invoice", vec!["a@b.com".into()])
    ///     .text_body("See attached.")
    ///     .attach("invoice.pdf", "application/pdf", pdf);
    /// ```
    pub fn attach<F: Into<String>, C: Into<String>>(
        mut self,
        filename: F,
        content_type: C,
        data: Vec<u8>,
    ) -> Self {
        self.attachments
            .push(Attachment::new(filename, content_type, data));
        self
    }
}

// =========================================================================
// Errors
// =========================================================================

/// Errors any send / render path can return.
#[derive(Debug)]
pub enum EmailError {
    /// `EmailMessage.from` was empty and no `email_default_from`
    /// setting is configured.
    MissingFrom,
    /// `EmailMessage.to` was empty. Sending to no one is always a
    /// programming error.
    NoRecipients,
    /// A header value (From / To / Reply-To) didn't parse as an
    /// RFC 5322 address.
    Address(lettre::address::AddressError),
    /// `lettre::Message::builder()` rejected the composed message.
    /// Most commonly a malformed body or header combination.
    Build(lettre::error::Error),
    /// The SMTP transport failed: connection, TLS, auth, or relay
    /// reject. v1 does not retry; callers wanting durability enqueue
    /// the send via `umbra-tasks`.
    Smtp(lettre::transport::smtp::Error),
    /// Rendering an email body template failed.
    Templates(TemplateError),
    /// An attachment's `content_type` didn't parse as a valid MIME
    /// type. The filename is carried so the user-facing message
    /// names which attachment is bad.
    InvalidAttachmentContentType {
        filename: String,
        content_type: String,
    },
    /// The email subject (or another user-supplied header value)
    /// contains a CR (`\r`), LF (`\n`), NUL (`\x00`), or any other
    /// ASCII control character that is banned in RFC 5322 header
    /// fields. This is the SMTP header-injection / Bcc-injection
    /// guard. Pass a clean subject string.
    InvalidHeaderValue {
        field: &'static str,
        offending_char: char,
    },
    /// The API backend was selected but `email_api_provider` and/or
    /// `email_api_key` is missing or invalid. Set both
    /// (`email_api_provider = "resend" | "sendgrid"`, `email_api_key =
    /// "<key>"`), or unset `UMBRA_EMAIL_BACKEND=api`.
    ApiNotConfigured,
    /// The HTTP request to the provider failed at the transport level
    /// (DNS, connection, TLS, timeout) before any response was received.
    /// Carries the provider's error message.
    ApiTransport(String),
    /// The provider returned a non-2xx HTTP status. Carries the status
    /// code and the response body so the operator can see the provider's
    /// own error description (bad key, malformed payload, rate limit).
    ApiResponse { status: u16, body: String },
    /// The console backend was used in a non-Dev/Test environment.
    /// Printing full message bodies (including password-reset tokens)
    /// to stderr/stdout in production would leak secrets to log
    /// aggregators. Configure `email_smtp_host` for production, or
    /// set `UMBRA_EMAIL_BACKEND=console` explicitly if you understand
    /// the risk and are intentionally forcing console mode.
    ConsoleBackendInProduction,
}

impl std::fmt::Display for EmailError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EmailError::MissingFrom => write!(
                f,
                "umbra-email: missing From and no email_default_from configured"
            ),
            EmailError::NoRecipients => {
                write!(f, "umbra-email: EmailMessage.to is empty")
            }
            EmailError::Address(e) => write!(f, "umbra-email: address: {e}"),
            EmailError::Build(e) => write!(f, "umbra-email: message build: {e}"),
            EmailError::Smtp(e) => write!(f, "umbra-email: smtp: {e}"),
            EmailError::Templates(e) => write!(f, "umbra-email: templates: {e}"),
            EmailError::InvalidAttachmentContentType {
                filename,
                content_type,
            } => write!(
                f,
                "umbra-email: attachment `{filename}` has invalid content type `{content_type}`",
            ),
            EmailError::InvalidHeaderValue {
                field,
                offending_char,
            } => write!(
                f,
                "umbra-email: {field} contains a forbidden control character \
                 U+{:04X} (CRLF/LF/CR/NUL in a header value is an SMTP \
                 injection vector)",
                *offending_char as u32,
            ),
            EmailError::ApiNotConfigured => write!(
                f,
                "umbra-email: API backend selected but email_api_provider \
                 and/or email_api_key is missing — set both, or unset \
                 UMBRA_EMAIL_BACKEND=api",
            ),
            EmailError::ApiTransport(e) => {
                write!(f, "umbra-email: API HTTP transport: {e}")
            }
            EmailError::ApiResponse { status, body } => write!(
                f,
                "umbra-email: API provider returned HTTP {status}: {body}",
            ),
            EmailError::ConsoleBackendInProduction => write!(
                f,
                "umbra-email: console backend refused to send in a non-Dev/Test \
                 environment — printing email bodies (including tokens) to stderr \
                 leaks secrets to log aggregators. Configure `email_smtp_host` for \
                 production, or set `UMBRA_EMAIL_BACKEND=console` to opt in explicitly.",
            ),
        }
    }
}

impl std::error::Error for EmailError {}

impl From<lettre::address::AddressError> for EmailError {
    fn from(e: lettre::address::AddressError) -> Self {
        Self::Address(e)
    }
}

impl From<lettre::error::Error> for EmailError {
    fn from(e: lettre::error::Error) -> Self {
        Self::Build(e)
    }
}

impl From<lettre::transport::smtp::Error> for EmailError {
    fn from(e: lettre::transport::smtp::Error) -> Self {
        Self::Smtp(e)
    }
}

impl From<TemplateError> for EmailError {
    fn from(e: TemplateError) -> Self {
        Self::Templates(e)
    }
}

// =========================================================================
// Config
// =========================================================================

/// Cached, parsed view of the SMTP / backend settings. Read once at
/// first use and pinned for the process lifetime. A `Settings` reload
/// would require restarting the process, which matches every other
/// ambient handle in umbra.
#[derive(Debug, Clone)]
struct EmailConfig {
    backend: BackendKind,
    /// Host. None ⇒ console mode regardless of port / creds.
    smtp_host: Option<String>,
    smtp_port: u16,
    smtp_user: Option<String>,
    smtp_password: Option<String>,
    default_from: Option<String>,
    /// HTTP API provider, when the API backend is selected.
    api_provider: Option<EmailApiProvider>,
    /// API key (bearer token) for the HTTP API provider.
    api_key: Option<String>,
    /// Per-send timeout passed to lettre. Covers connection +
    /// SMTP command exchange. Configurable via `email_smtp_timeout_secs`
    /// in settings; defaults to 10 s. Set to 0 to remove the cap
    /// (not recommended in production).
    smtp_timeout_secs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackendKind {
    /// HTTP API backend (Resend / SendGrid) — POSTs the message as JSON
    /// to a transactional-email provider over HTTPS.
    Api,
    Smtp,
    Console,
}

/// A transactional-email HTTP API provider. Both expose a simple JSON
/// `POST` endpoint authenticated with a bearer token. Selected via the
/// `email_api_provider` setting (`"resend"` / `"sendgrid"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmailApiProvider {
    /// Resend (<https://resend.com>). `POST https://api.resend.com/emails`.
    Resend,
    /// SendGrid (<https://sendgrid.com>). `POST https://api.sendgrid.com/v3/mail/send`.
    SendGrid,
}

impl EmailApiProvider {
    /// Parse the `email_api_provider` setting value. Case-insensitive.
    fn from_setting(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "resend" => Some(Self::Resend),
            "sendgrid" => Some(Self::SendGrid),
            _ => None,
        }
    }
}

static CONFIG: OnceLock<EmailConfig> = OnceLock::new();

fn config() -> &'static EmailConfig {
    CONFIG.get_or_init(load_config)
}

fn load_config() -> EmailConfig {
    // Env-var override wins over settings, same precedence as every
    // other UMBRA_-prefixed knob.
    let backend_override = std::env::var("UMBRA_EMAIL_BACKEND").ok();
    let env_forced_console = backend_override
        .as_deref()
        .map(|v| v.eq_ignore_ascii_case("console"))
        .unwrap_or(false);
    let env_forced_api = backend_override
        .as_deref()
        .map(|v| v.eq_ignore_ascii_case("api"))
        .unwrap_or(false);

    // Re-parse from env / umbra.toml rather than reaching for the
    // ambient `settings::get()`, which isn't on the facade and would
    // panic before `App::build()` runs anyway. `Settings::from_env`
    // is a pure function over env + cwd that the App also calls at
    // boot, so the two views agree.
    let settings = umbra::Settings::from_env().ok();
    let extra = settings.as_ref().map(|s| &s.extra);

    let smtp_host = extra
        .and_then(|e| e.get("email_smtp_host"))
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let smtp_port = extra
        .and_then(|e| e.get("email_smtp_port"))
        .and_then(|v| v.as_integer())
        .and_then(|n| u16::try_from(n).ok())
        .unwrap_or(587);

    let smtp_user = extra
        .and_then(|e| e.get("email_smtp_user"))
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let smtp_password = extra
        .and_then(|e| e.get("email_smtp_password"))
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let default_from = extra
        .and_then(|e| e.get("email_default_from"))
        .and_then(|v| v.as_str())
        .map(str::to_string);

    // 10 s is tight enough to surface a hung relay without stalling a
    // request for longer than a user will tolerate. Override with
    // `email_smtp_timeout_secs = N` in umbra.toml or
    // `UMBRA_EMAIL_SMTP_TIMEOUT_SECS=N` in the environment. Set to 0
    // to remove the cap entirely (not recommended in production).
    let smtp_timeout_secs = extra
        .and_then(|e| e.get("email_smtp_timeout_secs"))
        .and_then(|v| v.as_integer())
        .and_then(|n| u64::try_from(n).ok())
        .unwrap_or(10);

    let api_provider = extra
        .and_then(|e| e.get("email_api_provider"))
        .and_then(|v| v.as_str())
        .and_then(EmailApiProvider::from_setting);

    let api_key = extra
        .and_then(|e| e.get("email_api_key"))
        .and_then(|v| v.as_str())
        .map(str::to_string);

    // Resolution order: API (env-forced, or provider+key both set) →
    // SMTP (host set) → console. `UMBRA_EMAIL_BACKEND=console` still
    // forces console above everything, preserving the existing safety
    // valve for CI / tests.
    let api_selected =
        !env_forced_console && (env_forced_api || (api_provider.is_some() && api_key.is_some()));

    let backend = if env_forced_console {
        BackendKind::Console
    } else if api_selected {
        BackendKind::Api
    } else if smtp_host.is_some() {
        BackendKind::Smtp
    } else {
        BackendKind::Console
    };

    EmailConfig {
        backend,
        smtp_host,
        smtp_port,
        smtp_user,
        smtp_password,
        default_from,
        api_provider,
        api_key,
        smtp_timeout_secs,
    }
}

// =========================================================================
// Send
// =========================================================================

/// Send an email through the configured backend. Returns `Ok(())` on
/// successful handoff (the SMTP relay accepted the message, or the
/// console backend printed it). Does not block on remote delivery.
pub async fn send(message: &EmailMessage) -> Result<(), EmailError> {
    if message.to.is_empty() {
        return Err(EmailError::NoRecipients);
    }

    let cfg = config();

    // Resolve the From: explicit message field beats the default-from
    // setting beats nothing.
    let from = if !message.from.is_empty() {
        message.from.clone()
    } else if let Some(default) = cfg.default_from.as_deref() {
        default.to_string()
    } else {
        return Err(EmailError::MissingFrom);
    };

    // The API backend builds its own JSON body from the EmailMessage —
    // it never goes through lettre's MIME composition. Validate the
    // header values (the injection guard) but skip `compose`, which
    // targets the SMTP wire format.
    if cfg.backend == BackendKind::Api {
        validate_header_value("subject", &message.subject)?;
        validate_header_value("from", &from)?;
        if let Some(reply_to) = &message.reply_to {
            validate_header_value("reply_to", reply_to)?;
        }
        for recipient in &message.to {
            validate_header_value("to", recipient)?;
        }
        let provider = cfg.api_provider.ok_or(EmailError::ApiNotConfigured)?;
        let key = cfg.api_key.as_deref().ok_or(EmailError::ApiNotConfigured)?;
        let request = build_api_request(provider, key, message, &from);
        return deliver_api(request).await;
    }

    let composed = compose(&from, message)?;

    match cfg.backend {
        BackendKind::Api => unreachable!("API backend handled above"),
        BackendKind::Console => {
            // The console backend prints the full rendered RFC 822
            // message — headers AND body — to stderr. In Dev / Test
            // that is the intended developer-visibility behaviour.
            // In production it would leak password-reset tokens or
            // magic-link URLs to log aggregators.
            //
            // Fail-closed in non-Dev/Test: return a clear error
            // instead of printing the body, so the operator knows
            // exactly why mail was refused and what to fix.
            //
            // `get_opt` not `get`: sending mail before `App::build`
            // initialises settings (a worker bootstrap, a test) must not
            // panic. With settings absent we treat the environment as
            // unknown and take the safe path: refuse to print secrets.
            let env = umbra::settings::get_opt().map(|s| s.environment.clone());
            let is_dev_or_test = matches!(
                env,
                Some(umbra::Environment::Dev) | Some(umbra::Environment::Test)
            );
            if !is_dev_or_test {
                tracing::error!(
                    "umbra-email: console backend refused to deliver in a non-Dev/Test \
                     environment. Configure `email_smtp_host` for production.",
                );
                return Err(EmailError::ConsoleBackendInProduction);
            }
            ConsoleBackend.deliver(&composed, message)
        }
        BackendKind::Smtp => deliver_smtp(cfg, composed).await,
    }
}

/// Validate a user-supplied header value against the characters that are
/// banned in RFC 5322 header fields.
///
/// RFC 5322 §2.2 / RFC 5321 §4.1.1 forbid bare CR (`\r`), bare LF
/// (`\n`), and NUL (`\x00`) in header values because they allow an
/// attacker to inject arbitrary headers — the classic SMTP
/// header-injection / Bcc-injection vector.  We also reject the full
/// C0 control range (< U+0020) except for horizontal tab (U+0009),
/// which RFC 5322 allows inside folded header values.
///
/// `lettre` 0.11 does not validate these characters itself, so this
/// function is the plugin's own gate.
fn validate_header_value(field: &'static str, value: &str) -> Result<(), EmailError> {
    for ch in value.chars() {
        // Allow printable ASCII + non-ASCII Unicode.
        // Allow horizontal tab (RFC 5322 permits it in folded headers).
        // Reject every other control character (< U+0020) plus DEL.
        let is_forbidden =
            matches!(ch, '\r' | '\n' | '\x00') || (ch < '\x20' && ch != '\t') || ch == '\x7f';
        if is_forbidden {
            return Err(EmailError::InvalidHeaderValue {
                field,
                offending_char: ch,
            });
        }
    }
    Ok(())
}

/// Compose an `EmailMessage` into a wire-ready `lettre::Message`.
///
/// The public bridge between the umbra type and the lettre type.
/// `send_email` calls this internally; downstream callers who want to
/// queue, sign, or introspect a message before delivery can call it
/// directly. `lettre::Message::formatted()` then yields the raw RFC
/// 822 / MIME bytes.
///
/// `from` is the envelope From address — typically pulled from the
/// `email_default_from` setting when `EmailMessage.from` is empty.
///
/// # Errors
///
/// Returns [`EmailError::InvalidHeaderValue`] if `message.subject`
/// (or any other user-supplied header string) contains a CR, LF, NUL,
/// or other ASCII control character banned by RFC 5322.  This is the
/// SMTP header-injection / Bcc-injection guard.  `lettre` 0.11 does
/// not perform this check itself.
pub fn compose(from: &str, message: &EmailMessage) -> Result<Message, EmailError> {
    // --- Header-injection guard (RFC 5322 / SMTP Bcc-injection) -------
    // Validate before touching the lettre builder so we get a typed,
    // descriptive error rather than a silently-accepted malicious message.
    validate_header_value("subject", &message.subject)?;
    // Also guard the From display name and the Reply-To display name.
    // The address *local-part* and *domain* are validated by lettre's
    // `Mailbox::parse`; the display name is free-form text that lettre
    // does not validate.
    validate_header_value("from", from)?;
    if let Some(reply_to) = &message.reply_to {
        validate_header_value("reply_to", reply_to)?;
    }
    // Individual recipient addresses are parsed by lettre; guard the
    // whole address string here too (display names included).
    for recipient in &message.to {
        validate_header_value("to", recipient)?;
    }
    // ------------------------------------------------------------------

    let from_mbox: Mailbox = from.parse()?;
    let mut builder = Message::builder().from(from_mbox).subject(&message.subject);

    for recipient in &message.to {
        let to: Mailbox = recipient.parse()?;
        builder = builder.to(to);
    }

    if let Some(reply_to) = &message.reply_to {
        let mbox: Mailbox = reply_to.parse()?;
        builder = builder.reply_to(mbox);
    }

    // No attachments → use the existing single/alternative body
    // shape directly. Adding a multipart/mixed wrapper around a
    // single-part body would still validate but adds an extra MIME
    // level for no reason, so skip it when we can.
    if message.attachments.is_empty() {
        let composed = match (&message.text_body, &message.html_body) {
            (Some(text), Some(html)) => builder.multipart(MultiPart::alternative_plain_html(
                text.clone(),
                html.clone(),
            ))?,
            (Some(text), None) => builder.singlepart(SinglePart::plain(text.clone()))?,
            (None, Some(html)) => builder.singlepart(SinglePart::html(html.clone()))?,
            (None, None) => builder.singlepart(SinglePart::plain(String::new()))?,
        };
        return Ok(composed);
    }

    // Attachments present → wrap the body in `multipart/mixed`. The
    // body part is either a singlepart (one body) or a
    // multipart/alternative (both bodies) nested inside the mixed
    // envelope, per RFC 2046:
    //
    //   multipart/mixed
    //     ├── multipart/alternative
    //     │     ├── text/plain
    //     │     └── text/html
    //     ├── attachment 1
    //     └── attachment 2
    let mut mixed = MultiPart::mixed().build();
    let body_part = match (&message.text_body, &message.html_body) {
        (Some(text), Some(html)) => {
            // alternative as a nested multipart inside mixed.
            mixed = mixed.multipart(MultiPart::alternative_plain_html(
                text.clone(),
                html.clone(),
            ));
            None
        }
        (Some(text), None) => Some(SinglePart::plain(text.clone())),
        (None, Some(html)) => Some(SinglePart::html(html.clone())),
        (None, None) => Some(SinglePart::plain(String::new())),
    };
    if let Some(part) = body_part {
        mixed = mixed.singlepart(part);
    }

    for att in &message.attachments {
        mixed = mixed.singlepart(build_attachment_part(att)?);
    }

    Ok(builder.multipart(mixed)?)
}

/// Render one [`Attachment`] into a `SinglePart` with the right
/// Content-Type and Content-Disposition headers. lettre handles the
/// base64 encoding when the part lands in the multipart/mixed tree.
fn build_attachment_part(att: &Attachment) -> Result<SinglePart, EmailError> {
    let ct: ContentType =
        att.content_type
            .parse()
            .map_err(|_| EmailError::InvalidAttachmentContentType {
                filename: att.filename.clone(),
                content_type: att.content_type.clone(),
            })?;
    let disposition = ContentDisposition::attachment(&att.filename);
    Ok(SinglePart::builder()
        .header(ct)
        .header(disposition)
        .body(att.data.clone()))
}

async fn deliver_smtp(cfg: &EmailConfig, message: Message) -> Result<(), EmailError> {
    use std::time::Duration;

    let host = cfg
        .smtp_host
        .as_deref()
        .expect("BackendKind::Smtp implies smtp_host is set");

    // STARTTLS on the submission port (587) is the right default. A
    // future setting can switch to implicit TLS (`smtps`, port 465)
    // when a deployment needs it.
    let mut transport =
        AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host)?.port(cfg.smtp_port);

    if let (Some(user), Some(pass)) = (cfg.smtp_user.as_deref(), cfg.smtp_password.as_deref()) {
        transport = transport.credentials(Credentials::new(user.to_string(), pass.to_string()));
    }

    // Apply the configurable send timeout. A value of 0 removes the cap
    // entirely (lettre interprets `None` as no timeout), which is not
    // recommended in production. The default (10 s) is tight enough to
    // surface a hung relay without stalling a request indefinitely.
    let timeout = if cfg.smtp_timeout_secs == 0 {
        None
    } else {
        Some(Duration::from_secs(cfg.smtp_timeout_secs))
    };
    transport = transport.timeout(timeout);

    let transport = transport.build();
    transport.send(message).await?;
    Ok(())
}

// =========================================================================
// HTTP API backend (Resend / SendGrid)
// =========================================================================

/// A fully-built provider HTTP request: the endpoint URL, the bearer
/// token, and the JSON body. Produced by [`build_api_request`] (a pure
/// function with no I/O, so the request mapping is unit-testable without
/// a network round-trip) and consumed by [`deliver_api`], which performs
/// the actual `reqwest` POST under the `api` feature.
#[derive(Debug, Clone)]
pub struct ApiRequest {
    /// Provider endpoint to POST to.
    pub url: String,
    /// Bearer token sent as `Authorization: Bearer <bearer>`.
    pub bearer: String,
    /// JSON request body in the provider's expected shape.
    pub body: serde_json::Value,
}

/// Map an [`EmailMessage`] into a provider-specific HTTP request.
///
/// Pure: builds the URL, bearer header, and JSON body from the message
/// alone, with no network call. `default_from` is the already-resolved
/// sender (the caller applies the `email_default_from` fallback before
/// calling this), and is used as `from` when the message's own `from`
/// is empty.
///
/// The two supported providers expect different JSON shapes:
///
/// - **Resend** (`POST https://api.resend.com/emails`):
///   `{ from, to: [..], subject, html?, text? }`.
/// - **SendGrid** (`POST https://api.sendgrid.com/v3/mail/send`):
///   `{ personalizations: [{ to: [{email}] }], from: {email}, subject,
///   content: [{type, value}] }`.
///
/// Attachments are included as base64 in the JSON body for both
/// providers (Resend: `attachments: [{filename, content}]`; SendGrid:
/// `attachments: [{filename, type, content, disposition}]`).
pub fn build_api_request(
    provider: EmailApiProvider,
    key: &str,
    msg: &EmailMessage,
    default_from: &str,
) -> ApiRequest {
    use base64::Engine as _;

    let from = if msg.from.is_empty() {
        default_from
    } else {
        msg.from.as_str()
    };

    match provider {
        EmailApiProvider::Resend => {
            let mut body = serde_json::json!({
                "from": from,
                "to": msg.to,
                "subject": msg.subject,
            });
            if let Some(html) = &msg.html_body {
                body["html"] = serde_json::Value::String(html.clone());
            }
            if let Some(text) = &msg.text_body {
                body["text"] = serde_json::Value::String(text.clone());
            }
            if let Some(reply_to) = &msg.reply_to {
                body["reply_to"] = serde_json::Value::String(reply_to.clone());
            }
            if !msg.attachments.is_empty() {
                let attachments: Vec<serde_json::Value> = msg
                    .attachments
                    .iter()
                    .map(|a| {
                        serde_json::json!({
                            "filename": a.filename,
                            "content": base64::engine::general_purpose::STANDARD.encode(&a.data),
                        })
                    })
                    .collect();
                body["attachments"] = serde_json::Value::Array(attachments);
            }
            ApiRequest {
                url: "https://api.resend.com/emails".to_string(),
                bearer: key.to_string(),
                body,
            }
        }
        EmailApiProvider::SendGrid => {
            let to: Vec<serde_json::Value> = msg
                .to
                .iter()
                .map(|addr| serde_json::json!({ "email": addr }))
                .collect();

            let mut content: Vec<serde_json::Value> = Vec::new();
            // SendGrid requires text/plain to precede text/html when both
            // are present (RFC 2046 "least to most faithful" ordering).
            if let Some(text) = &msg.text_body {
                content.push(serde_json::json!({ "type": "text/plain", "value": text }));
            }
            if let Some(html) = &msg.html_body {
                content.push(serde_json::json!({ "type": "text/html", "value": html }));
            }
            if content.is_empty() {
                // SendGrid rejects an empty content array; send an empty
                // plain part so a body-less message still validates.
                content.push(serde_json::json!({ "type": "text/plain", "value": "" }));
            }

            let mut body = serde_json::json!({
                "personalizations": [{ "to": to }],
                "from": { "email": from },
                "subject": msg.subject,
                "content": content,
            });
            if let Some(reply_to) = &msg.reply_to {
                body["reply_to"] = serde_json::json!({ "email": reply_to });
            }
            if !msg.attachments.is_empty() {
                let attachments: Vec<serde_json::Value> = msg
                    .attachments
                    .iter()
                    .map(|a| {
                        serde_json::json!({
                            "filename": a.filename,
                            "type": a.content_type,
                            "disposition": "attachment",
                            "content": base64::engine::general_purpose::STANDARD.encode(&a.data),
                        })
                    })
                    .collect();
                body["attachments"] = serde_json::Value::Array(attachments);
            }
            ApiRequest {
                url: "https://api.sendgrid.com/v3/mail/send".to_string(),
                bearer: key.to_string(),
                body,
            }
        }
    }
}

/// POST a [`build_api_request`] result to the provider. Maps a non-2xx
/// response (including the provider's response body) to
/// [`EmailError::ApiResponse`] and a transport failure to
/// [`EmailError::ApiTransport`].
///
/// Only compiled under the `api` feature (which pulls in `reqwest`).
#[cfg(feature = "api")]
async fn deliver_api(request: ApiRequest) -> Result<(), EmailError> {
    let client = reqwest::Client::new();
    let response = client
        .post(&request.url)
        .bearer_auth(&request.bearer)
        .json(&request.body)
        .send()
        .await
        .map_err(|e| EmailError::ApiTransport(e.to_string()))?;

    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let body = response
        .text()
        .await
        .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));
    Err(EmailError::ApiResponse {
        status: status.as_u16(),
        body,
    })
}

/// Without the `api` feature, selecting the API backend is a
/// configuration error surfaced at send time rather than a compile
/// error: the rest of the plugin (SMTP / console) still builds. Enable
/// `umbra-email/api` to compile the `reqwest`-backed delivery path.
#[cfg(not(feature = "api"))]
async fn deliver_api(_request: ApiRequest) -> Result<(), EmailError> {
    Err(EmailError::ApiTransport(
        "the `api` feature is not enabled — rebuild umbra-email with \
         `--features api` to use the HTTP API backend"
            .to_string(),
    ))
}

// =========================================================================
// Console backend
// =========================================================================

/// The dev / test backend. Prints the rendered message to stderr.
/// Picked automatically when `email_smtp_host` is unset, or when
/// `UMBRA_EMAIL_BACKEND=console` is in the environment.
#[derive(Debug, Default, Clone, Copy)]
pub struct ConsoleBackend;

impl ConsoleBackend {
    /// Print the composed message to stderr. Always succeeds; the
    /// signature returns `Result` for parity with the SMTP path.
    pub fn deliver(&self, message: &Message, original: &EmailMessage) -> Result<(), EmailError> {
        // lettre's formatted output is RFC-shaped bytes (CRLF, MIME
        // boundaries). Printing it raw is the most useful form for a
        // developer debugging a flow. Copy-paste it into a real
        // client to verify it renders.
        let formatted = message.formatted();
        let body = String::from_utf8_lossy(&formatted);
        eprintln!("---- umbra-email (console backend) ----");
        eprintln!("To: {}", original.to.join(", "));
        eprintln!("Subject: {}", original.subject);
        eprintln!("---- raw message ----");
        eprintln!("{body}");
        eprintln!("---- end ----");
        Ok(())
    }
}

// =========================================================================
// Templates
// =========================================================================

/// Render an email body via the framework template engine. Thin
/// wrapper around [`umbra::templates::render`] so callers get an
/// [`EmailError`] instead of a [`TemplateError`] (lets `?` flow
/// straight out of a send function).
///
/// Convention: text bodies live at `email/<name>.txt`, HTML bodies at
/// `email/<name>.html`. Call this once per part.
pub fn render_email_body<C: Serialize>(
    template_name: &str,
    context: &C,
) -> Result<String, EmailError> {
    Ok(umbra::templates::render(template_name, context)?)
}

// =========================================================================
// Test-only helpers
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_name_is_email() {
        assert_eq!(EmailPlugin.name(), "email");
    }

    #[test]
    fn missing_from_surfaces_as_a_specific_error() {
        let err = EmailError::MissingFrom;
        assert!(format!("{err}").contains("missing From"));
    }

    #[test]
    fn no_recipients_surfaces_as_a_specific_error() {
        let err = EmailError::NoRecipients;
        assert!(format!("{err}").contains("empty"));
    }

    /// `ConsoleBackendInProduction` must have a human-readable Display
    /// that names the problem and the fix, but must NOT carry any
    /// message body — the whole point is that we never print secrets.
    #[test]
    fn console_backend_in_production_error_display_names_the_problem() {
        let err = EmailError::ConsoleBackendInProduction;
        let msg = format!("{err}");
        assert!(
            msg.contains("console backend refused"),
            "display should name the refusal; got: {msg}"
        );
        assert!(
            msg.contains("email_smtp_host"),
            "display should tell the operator what to configure; got: {msg}"
        );
    }

    /// `load_config` must default `smtp_timeout_secs` to 10 when no
    /// `email_smtp_timeout_secs` key is present in the environment.
    /// This is the structural proof that every SMTP send has a timeout
    /// floor (a real hung-server integration test isn't feasible in
    /// unit-test scope, but if the default is 0 a hung relay would
    /// block indefinitely).
    // ---- HTTP API backend: build_api_request -------------------------

    #[test]
    fn resend_request_maps_url_bearer_and_body() {
        let msg = EmailMessage::new("Hello", vec!["alice@example.com".into()])
            .from("noreply@acme.test")
            .html_body("<p>hi</p>")
            .text_body("hi");
        let req = build_api_request(EmailApiProvider::Resend, "re_test_key", &msg, "fallback@x.test");

        assert_eq!(req.url, "https://api.resend.com/emails");
        assert_eq!(req.bearer, "re_test_key");
        assert_eq!(req.body["from"], "noreply@acme.test");
        assert_eq!(req.body["subject"], "Hello");
        // `to` is an array.
        assert_eq!(
            req.body["to"],
            serde_json::json!(["alice@example.com"]),
            "to must be a JSON array"
        );
        assert_eq!(req.body["html"], "<p>hi</p>");
        assert_eq!(req.body["text"], "hi");
    }

    #[test]
    fn resend_request_uses_default_from_when_message_from_empty() {
        // No `.from(...)` set on the message → falls back to default_from.
        let msg = EmailMessage::new("Subj", vec!["a@b.test".into()]).text_body("body");
        let req = build_api_request(EmailApiProvider::Resend, "k", &msg, "default@acme.test");
        assert_eq!(
            req.body["from"], "default@acme.test",
            "empty message.from must fall back to default_from"
        );
    }

    #[test]
    fn resend_request_maps_multiple_recipients_to_array() {
        let msg = EmailMessage::new("Subj", vec!["a@b.test".into(), "c@d.test".into()])
            .from("s@acme.test")
            .text_body("hi");
        let req = build_api_request(EmailApiProvider::Resend, "k", &msg, "x@x.test");
        assert_eq!(
            req.body["to"],
            serde_json::json!(["a@b.test", "c@d.test"]),
            "both recipients must be in the to array"
        );
    }

    #[test]
    fn sendgrid_request_maps_nested_shape() {
        let msg = EmailMessage::new("Hello", vec!["alice@example.com".into()])
            .from("noreply@acme.test")
            .html_body("<p>hi</p>")
            .text_body("hi");
        let req =
            build_api_request(EmailApiProvider::SendGrid, "SG.test_key", &msg, "fallback@x.test");

        assert_eq!(req.url, "https://api.sendgrid.com/v3/mail/send");
        assert_eq!(req.bearer, "SG.test_key");
        // from is nested under {email}.
        assert_eq!(req.body["from"]["email"], "noreply@acme.test");
        assert_eq!(req.body["subject"], "Hello");
        // personalizations[0].to is [{email}].
        assert_eq!(
            req.body["personalizations"][0]["to"],
            serde_json::json!([{ "email": "alice@example.com" }]),
        );
        // content has text/plain then text/html.
        assert_eq!(req.body["content"][0]["type"], "text/plain");
        assert_eq!(req.body["content"][0]["value"], "hi");
        assert_eq!(req.body["content"][1]["type"], "text/html");
        assert_eq!(req.body["content"][1]["value"], "<p>hi</p>");
    }

    #[test]
    fn sendgrid_request_maps_multiple_recipients() {
        let msg = EmailMessage::new("Subj", vec!["a@b.test".into(), "c@d.test".into()])
            .from("s@acme.test")
            .text_body("hi");
        let req = build_api_request(EmailApiProvider::SendGrid, "k", &msg, "x@x.test");
        assert_eq!(
            req.body["personalizations"][0]["to"],
            serde_json::json!([{ "email": "a@b.test" }, { "email": "c@d.test" }]),
        );
    }

    #[test]
    fn sendgrid_uses_default_from_when_message_from_empty() {
        let msg = EmailMessage::new("Subj", vec!["a@b.test".into()]).text_body("body");
        let req = build_api_request(EmailApiProvider::SendGrid, "k", &msg, "default@acme.test");
        assert_eq!(req.body["from"]["email"], "default@acme.test");
    }

    #[test]
    fn api_provider_parses_case_insensitively() {
        assert_eq!(
            EmailApiProvider::from_setting("Resend"),
            Some(EmailApiProvider::Resend)
        );
        assert_eq!(
            EmailApiProvider::from_setting("SENDGRID"),
            Some(EmailApiProvider::SendGrid)
        );
        assert_eq!(EmailApiProvider::from_setting("mailgun"), None);
    }

    #[test]
    fn smtp_timeout_default_is_ten_seconds() {
        // Exercise load_config in isolation. The CONFIG OnceLock may
        // already be set in this process from another test (cargo test
        // runs unit tests in one binary), so we call load_config()
        // directly rather than going through config().
        // Ensure the key is absent so we get the pure default.
        unsafe {
            std::env::remove_var("UMBRA_EMAIL_SMTP_TIMEOUT_SECS");
        }
        let cfg = load_config();
        assert_eq!(
            cfg.smtp_timeout_secs, 10,
            "smtp_timeout_secs should default to 10 s; got {}",
            cfg.smtp_timeout_secs
        );
    }
}
