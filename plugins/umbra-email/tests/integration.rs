//! End-to-end coverage for umbra-email.
//!
//! Boots once via OnceCell with `EmailPlugin` registered against a
//! tempfile-backed sqlite (the default pool the App requires, even
//! though this plugin doesn't touch it). A `templates/` directory is
//! materialised under a TempDir so the `render_email_body` test can
//! exercise the framework template engine against a real file.
//!
//! Every test below relies on the console backend: tests never set
//! `email_smtp_host`, so the lazy `EmailConfig::load_config` resolves
//! to `BackendKind::Console` and `send` prints to stderr instead of
//! reaching for a relay.

use tokio::sync::OnceCell;

use umbra_email::{EmailError, EmailMessage, EmailPlugin, render_email_body, send};

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        // Belt and braces: clear any SMTP host that might leak in
        // from the developer's shell so the config resolves to
        // console mode. The CONFIG OnceLock is loaded lazily on
        // first `send` / `config()` call, so clearing it here is
        // enough.
        // SAFETY: this runs once before any thread that reads the
        // env var below.
        unsafe {
            std::env::remove_var("UMBRA_EMAIL_SMTP_HOST");
            std::env::remove_var("UMBRA_EMAIL_DEFAULT_FROM");
        }

        let settings = umbra::Settings::from_env().expect("figment defaults load");
        let tmp = tempfile::tempdir().expect("tempdir");

        // Lay down a templates tree under the tempdir so the engine
        // can find `email/welcome.txt` at boot.
        let templates_root = tmp.path().join("templates");
        let email_dir = templates_root.join("email");
        std::fs::create_dir_all(&email_dir).expect("mkdir templates/email");
        std::fs::write(
            email_dir.join("welcome.txt"),
            "Hi {{ name }}, welcome to {{ site }}.\n",
        )
        .expect("write welcome.txt");

        // Hold the tempdir for the process lifetime. App publishes
        // ambient state pointing into it.
        std::mem::forget(tmp);

        // In-memory sqlite is fine: this plugin doesn't touch the
        // DB, but App::build() requires a default pool.
        let pool = umbra::db::connect("sqlite::memory:")
            .await
            .expect("sqlite in-memory pool");

        umbra::App::builder()
            .settings(settings)
            .database("default", pool)
            .templates_dir(&templates_root)
            .plugin(EmailPlugin)
            .build()
            .expect("App::build with EmailPlugin");
    })
    .await;
}

/// With no `email_smtp_host` in the environment, the config resolves
/// to the console backend and `send` succeeds without ever opening a
/// socket. This is the dev-mode happy path: `cargo run` exercises a
/// welcome / password-reset flow with zero SMTP setup.
#[tokio::test]
async fn console_backend_is_default_when_smtp_host_is_unset() {
    boot().await;
    let msg = EmailMessage::new("Welcome", vec!["alice@example.com".into()])
        .from("noreply@example.com")
        .text_body("Hi there.");
    send(&msg).await.expect("console send should succeed");
}

/// `render_email_body` flows through `umbra::templates::render` and
/// substitutes context variables into the body text. Confirms the
/// plugin's render wrapper is a real call into the engine, not a
/// stub.
#[tokio::test]
async fn render_email_body_uses_the_framework_template_engine() {
    boot().await;
    let ctx = umbra::templates::context!(name => "Alice", site => "umbra");
    let body = render_email_body("email/welcome.txt", &ctx).expect("render");
    assert!(
        body.contains("Hi Alice, welcome to umbra."),
        "rendered body should carry the substituted values, got: {body:?}"
    );
}

/// An empty `to` is a programming error, not a recoverable runtime
/// case. `send` returns `EmailError::NoRecipients` rather than
/// silently doing nothing.
#[tokio::test]
async fn missing_recipients_returns_an_error_variant() {
    boot().await;
    let msg = EmailMessage::new("Subject", vec![]).from("noreply@example.com");
    let err = send(&msg).await.expect_err("empty `to` should error");
    assert!(
        matches!(err, EmailError::NoRecipients),
        "expected NoRecipients, got {err:?}"
    );
}

/// Empty `from` with no `email_default_from` configured surfaces as
/// `MissingFrom`. Mirrors Django's "either configure DEFAULT_FROM_EMAIL
/// or specify a from on the message" contract.
#[tokio::test]
async fn missing_from_returns_an_error_when_default_is_also_unset() {
    boot().await;
    let msg = EmailMessage::new("Subject", vec!["bob@example.com".into()]).text_body("Hi");
    let err = send(&msg).await.expect_err("missing From should error");
    assert!(
        matches!(err, EmailError::MissingFrom),
        "expected MissingFrom, got {err:?}"
    );
}

/// EmailPlugin reports `"email"`. The plugin doesn't contribute any
/// models, so it won't show up in `registered_plugins()` (that
/// accessor lists only plugins with models). The trait method on the
/// concrete plugin is the canonical assertion.
#[test]
fn email_plugin_registers_under_the_name_email() {
    use umbra::prelude::Plugin;
    assert_eq!(EmailPlugin.name(), "email");
}
