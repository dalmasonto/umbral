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
//!
//! The env var `UMBRA_EMAIL_BACKEND=console` forces the console
//! backend even when SMTP keys are present. Useful in CI / tests.
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
//! - No attachments, no inline images, no CC / BCC. Plain
//!   From / To / Subject / Reply-To / Body.
//! - No S/MIME or DKIM signing. Use your relay's signing.
//! - One recipient list (`to`). Multiple recipients work; CC and BCC
//!   do not.
//! - The console backend is the default when `email_smtp_host` is
//!   absent. The alternative (silently no-op) was rejected: a missing
//!   env var that drops a password reset on the floor is the kind of
//!   production footgun this plugin exists to avoid.

#![allow(clippy::result_large_err)]

use std::sync::OnceLock;

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackendKind {
    Smtp,
    Console,
}

static CONFIG: OnceLock<EmailConfig> = OnceLock::new();

fn config() -> &'static EmailConfig {
    CONFIG.get_or_init(load_config)
}

fn load_config() -> EmailConfig {
    // Env-var override wins over settings, same precedence as every
    // other UMBRA_-prefixed knob.
    let env_forced_console = std::env::var("UMBRA_EMAIL_BACKEND")
        .map(|v| v.eq_ignore_ascii_case("console"))
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

    let backend = if env_forced_console || smtp_host.is_none() {
        BackendKind::Console
    } else {
        BackendKind::Smtp
    };

    EmailConfig {
        backend,
        smtp_host,
        smtp_port,
        smtp_user,
        smtp_password,
        default_from,
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

    let composed = compose(&from, message)?;

    match cfg.backend {
        BackendKind::Console => ConsoleBackend.deliver(&composed, message),
        BackendKind::Smtp => deliver_smtp(cfg, composed).await,
    }
}

fn compose(from: &str, message: &EmailMessage) -> Result<Message, EmailError> {
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

    // Body shape: text-only, html-only, both (alternative), or empty.
    let message = match (&message.text_body, &message.html_body) {
        (Some(text), Some(html)) => builder.multipart(MultiPart::alternative_plain_html(
            text.clone(),
            html.clone(),
        ))?,
        (Some(text), None) => builder.singlepart(SinglePart::plain(text.clone()))?,
        (None, Some(html)) => builder.singlepart(SinglePart::html(html.clone()))?,
        (None, None) => builder.singlepart(SinglePart::plain(String::new()))?,
    };

    Ok(message)
}

async fn deliver_smtp(cfg: &EmailConfig, message: Message) -> Result<(), EmailError> {
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

    let transport = transport.build();
    transport.send(message).await?;
    Ok(())
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
}
