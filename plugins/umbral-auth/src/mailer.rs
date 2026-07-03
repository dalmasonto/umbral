//! The pluggable email seam. umbral-auth renders bodies via
//! `umbral::templates` and hands them to whatever `AuthMailer` the app wired
//! (default: print to stderr). Keeps auth decoupled from any mail crate.

use async_trait::async_trait;
use std::future::Future;
use std::sync::{Arc, OnceLock};

/// Which auth flow produced an [`OutgoingMail`], together with that flow's
/// raw data. Match on this in a custom [`AuthMailer`] to build the message
/// yourself — e.g. trigger your email provider's own template with the code
/// or reset URL as a variable — instead of forwarding the framework-rendered
/// bodies.
///
/// Marked `#[non_exhaustive]`: future auth flows (magic links, custom-action
/// notifications, …) add variants, so always include a `_ => { … }` arm when
/// you match on it.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum MailKind {
    /// Email-address verification. `code` is the plaintext 6-digit one-time
    /// code (it expires in 15 minutes; only its hash is stored server-side).
    EmailVerification { code: String },
    /// Password reset. `reset_url` is the tokenized link pointing at your
    /// reset page (it expires in 1 hour, single-use).
    PasswordReset { reset_url: String },
}

/// An auth email handed to the configured [`AuthMailer`].
///
/// It carries BOTH the framework-rendered bodies (`subject` / `html` / `text`,
/// produced from the overridable `templates/auth/email/*` templates) AND the
/// semantic [`MailKind`] plus recipient context. So a simple mailer can just
/// forward the rendered bodies to a transport, while a mailer that wants full
/// control can ignore them and build its own message from `kind`, `to`, and
/// `username` (e.g. call a transactional-email provider with a template id and
/// the verification code as a merge variable).
#[derive(Debug, Clone)]
pub struct OutgoingMail {
    /// Recipient email address.
    pub to: String,
    /// Recipient's username — handy for personalising a custom message.
    pub username: String,
    /// Which flow produced this email, plus its raw data (the verification
    /// code / the reset URL). Match on it to fully customise per email type.
    pub kind: MailKind,
    /// Framework-rendered subject line (from the overridable templates).
    pub subject: String,
    /// Framework-rendered HTML body (from the overridable templates).
    pub html: String,
    /// Framework-rendered plain-text body (from the overridable templates).
    pub text: String,
}

/// Failure to hand a message to the transport.
#[derive(Debug)]
pub enum AuthMailError {
    Send(String),
}
impl std::fmt::Display for AuthMailError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthMailError::Send(m) => write!(f, "failed to send auth email: {m}"),
        }
    }
}
impl std::error::Error for AuthMailError {}

/// What the app wires in. Implement for a type, or pass an async closure
/// `Fn(OutgoingMail) -> Future<Output = Result<(), AuthMailError>>` (blanket
/// impl below). Delegate to `umbral_email::send` in one line if you use it.
#[async_trait]
pub trait AuthMailer: Send + Sync {
    async fn send(&self, mail: OutgoingMail) -> Result<(), AuthMailError>;
}

#[async_trait]
impl<F, Fut> AuthMailer for F
where
    F: Fn(OutgoingMail) -> Fut + Send + Sync,
    Fut: Future<Output = Result<(), AuthMailError>> + Send,
{
    async fn send(&self, mail: OutgoingMail) -> Result<(), AuthMailError> {
        self(mail).await
    }
}

/// Default mailer: print the message to stderr (dev-visible code/link) and
/// log a loud warning if it's the active mailer outside Dev/Test.
pub struct ConsoleMailer;

/// Build the stderr block the [`ConsoleMailer`] prints for `mail`.
///
/// In Dev/Test (`prod == false`) the full rendered body is shown so a developer
/// can read the verification code / reset link straight from the console.
///
/// Outside Dev/Test (`prod == true`) the secret-bearing body is **suppressed** —
/// only the recipient and subject are printed. Without this, a deployment that
/// forgot to wire a real `AuthPlugin::mailer(...)` would emit live, single-use
/// reset tokens and verification codes into stdout/stderr log aggregation, where
/// anyone with log access could replay them (audit plugin-auth #7).
pub fn console_output(mail: &OutgoingMail, prod: bool) -> String {
    if prod {
        format!(
            "\n--- umbral-auth email (BODY SUPPRESSED: non-Dev/Test) ---\nTo: {}\nSubject: {}\n\n\
             [body withheld — the verification code / reset link is NOT printed outside Dev/Test; \
             wire AuthPlugin::mailer(...) to deliver it]\n-------------------------\n",
            mail.to, mail.subject
        )
    } else {
        format!(
            "\n--- umbral-auth email ---\nTo: {}\nSubject: {}\n\n{}\n-------------------------\n",
            mail.to, mail.subject, mail.text
        )
    }
}

#[async_trait]
impl AuthMailer for ConsoleMailer {
    async fn send(&self, mail: OutgoingMail) -> Result<(), AuthMailError> {
        let prod = umbral::settings::get_opt()
            .map(|s| {
                !matches!(
                    s.environment,
                    umbral::Environment::Dev | umbral::Environment::Test
                )
            })
            .unwrap_or(false);
        if prod {
            tracing::warn!(
                to = %mail.to,
                "umbral-auth ConsoleMailer is active in a non-Dev environment — auth emails are \
                 only printed (bodies suppressed), not delivered. Wire AuthPlugin::mailer(...) for \
                 production."
            );
        }
        eprint!("{}", console_output(&mail, prod));
        Ok(())
    }
}

static MAILER: OnceLock<Arc<dyn AuthMailer>> = OnceLock::new();

/// The mailer the flow functions use. Falls back to [`ConsoleMailer`].
/// Called by the email-verification and password-reset flow helpers
/// in `challenge.rs`.
pub(crate) fn active_mailer() -> Arc<dyn AuthMailer> {
    MAILER
        .get()
        .cloned()
        .unwrap_or_else(|| Arc::new(ConsoleMailer))
}

/// Install the process mailer. First call wins (mirrors the password policy
/// seal); `on_ready` calls this once at boot.
pub(crate) fn install_mailer(m: Arc<dyn AuthMailer>) {
    let _ = MAILER.set(m);
}
