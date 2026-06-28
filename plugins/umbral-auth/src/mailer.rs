//! The pluggable email seam. umbral-auth renders bodies via
//! `umbral::templates` and hands them to whatever `AuthMailer` the app wired
//! (default: print to stderr). Keeps auth decoupled from any mail crate.

use async_trait::async_trait;
use std::future::Future;
use std::sync::{Arc, OnceLock};

/// A rendered message ready to transmit.
#[derive(Debug, Clone)]
pub struct OutgoingMail {
    pub to: String,
    pub subject: String,
    pub html: String,
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
                 only printed, not delivered. Wire AuthPlugin::mailer(...) for production."
            );
        }
        eprintln!(
            "\n--- umbral-auth email ---\nTo: {}\nSubject: {}\n\n{}\n-------------------------\n",
            mail.to, mail.subject, mail.text
        );
        Ok(())
    }
}

static MAILER: OnceLock<Arc<dyn AuthMailer>> = OnceLock::new();

/// The mailer the flow functions use. Falls back to [`ConsoleMailer`].
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
