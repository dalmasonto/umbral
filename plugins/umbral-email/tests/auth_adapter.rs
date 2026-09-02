//! Behavioral coverage for `umbral_email::auth_mailer()` (gaps4 #82b): the
//! `AuthMailer` adapter that delegates umbral-auth's verification /
//! password-reset emails to this plugin's configured backend.
//!
//! Compiled only with `--features auth` (this crate's optional dep on
//! umbral-auth); without it the file is an empty crate.

#![cfg(feature = "auth")]

use tokio::sync::OnceCell;

use umbral_auth::{AuthMailer, MailKind, OutgoingMail};
use umbral_email::{EmailPlugin, auth_mailer};

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        // Belt and braces, mirroring tests/integration.rs: force the
        // console backend and give it a default From so a message with no
        // explicit `from` (exactly what the adapter sends) still resolves.
        unsafe {
            std::env::remove_var("UMBRAL_EMAIL_SMTP_HOST");
            std::env::set_var("UMBRAL_EMAIL_DEFAULT_FROM", "noreply@example.com");
        }

        let settings = umbral::Settings::from_env().expect("figment defaults load");
        let pool = umbral::db::connect_sqlite("sqlite::memory:")
            .await
            .expect("sqlite in-memory pool");

        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(EmailPlugin)
            .build()
            .expect("App::build with EmailPlugin");
    })
    .await;
}

fn sample_mail() -> OutgoingMail {
    OutgoingMail {
        to: "bob@example.com".to_string(),
        username: "bob".to_string(),
        kind: MailKind::EmailVerification {
            code: "042817".to_string(),
        },
        subject: "Verify your email".to_string(),
        html: "<p>042817</p>".to_string(),
        text: "Your code: 042817".to_string(),
    }
}

/// `auth_mailer()` satisfies `umbral_auth::AuthMailer` and, wired up like
/// `AuthPlugin::default().mailer(umbral_email::auth_mailer())`, sends
/// through this plugin's configured backend (console in this test) instead
/// of umbral-auth needing to know anything about SMTP or a mail crate.
#[tokio::test]
async fn auth_mailer_delegates_to_the_configured_backend() {
    boot().await;
    let mailer = auth_mailer();
    // Exercise it exactly the way umbral-auth's challenge.rs does:
    // `active_mailer().send(mail).await`, through the trait object.
    let boxed: std::sync::Arc<dyn AuthMailer> = std::sync::Arc::new(mailer);
    boxed
        .send(sample_mail())
        .await
        .expect("auth_mailer should deliver through the console backend");
}

/// Generic-over-`AuthMailer` call site (the shape `AuthPlugin::mailer`
/// actually takes: `impl AuthMailer + 'static`) compiles and works, proving
/// `AuthEmailMailer` is a genuine trait impl and not just an inherent
/// method with a similar name.
async fn send_through<M: AuthMailer>(mailer: &M, mail: OutgoingMail) {
    mailer
        .send(mail)
        .await
        .expect("delegated send should succeed");
}

#[tokio::test]
async fn auth_mailer_works_through_a_generic_authmailer_bound() {
    boot().await;
    send_through(&auth_mailer(), sample_mail()).await;
}
