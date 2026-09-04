//! gaps4 #91 — a custom `EmailBackend` registered via
//! `EmailPlugin::with_backend` takes over `send` for the whole app, so an app
//! can send through its own client (a SaaS API, an in-house relay, a test
//! spy) with just a struct + one trait impl + one builder call.
//!
//! Its own test binary with a single boot: the custom-backend registry and
//! the app's ambient state are both process-wide set-once, so everything runs
//! against ONE app build + ONE registered spy.

use std::sync::{Arc, Mutex};

use umbral_email::{EmailBackend, EmailError, EmailMessage, EmailPlugin, send};

/// A spy transport: records every message it's asked to deliver so the test
/// can prove `send` routed through it (and with what).
#[derive(Clone)]
struct SpyBackend {
    received: Arc<Mutex<Vec<EmailMessage>>>,
}

#[async_trait::async_trait]
impl EmailBackend for SpyBackend {
    async fn deliver(&self, message: &EmailMessage) -> Result<(), EmailError> {
        self.received.lock().unwrap().push(message.clone());
        Ok(())
    }
}

/// Build the app ONCE with the spy backend registered; hand back the shared
/// record of delivered messages.
async fn boot_with_spy() -> Arc<Mutex<Vec<EmailMessage>>> {
    // Clear any developer SMTP host so nothing but the spy is in play.
    // SAFETY: runs once before any thread reads this var.
    unsafe {
        std::env::remove_var("UMBRAL_EMAIL_SMTP_HOST");
    }
    let settings = umbral::Settings::from_env().expect("figment defaults load");
    let pool = umbral::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("sqlite in-memory pool");

    let received = Arc::new(Mutex::new(Vec::new()));
    let spy = SpyBackend {
        received: received.clone(),
    };

    umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .plugin(EmailPlugin::with_backend(spy))
        .build()
        .expect("App::build with a custom email backend");

    received
}

#[tokio::test]
async fn custom_backend_takes_over_send_and_keeps_the_injection_guard() {
    let received = boot_with_spy().await;

    // 1) A normal send must route through the spy (NOT console/SMTP), with the
    //    from resolved and every field intact.
    let msg = EmailMessage {
        from: "noreply@example.com".to_string(),
        to: vec!["ada@example.com".to_string()],
        subject: "Welcome to umbral".to_string(),
        text_body: Some("Hello Ada".to_string()),
        html_body: Some("<p>Hello Ada</p>".to_string()),
        ..Default::default()
    };
    send(&msg).await.expect("send via custom backend");

    {
        let got = received.lock().unwrap();
        let delivered = got
            .iter()
            .find(|m| m.subject == "Welcome to umbral")
            .expect("the custom backend must have received the welcome message");
        assert_eq!(delivered.from, "noreply@example.com", "from is resolved");
        assert_eq!(delivered.to, vec!["ada@example.com".to_string()]);
        assert_eq!(delivered.text_body.as_deref(), Some("Hello Ada"));
        assert_eq!(delivered.html_body.as_deref(), Some("<p>Hello Ada</p>"));
    }

    // 2) A CRLF-injected subject must be rejected BEFORE reaching the backend
    //    — a custom transport must never be handed unvalidated header values.
    let evil = EmailMessage {
        from: "noreply@example.com".to_string(),
        to: vec!["ada@example.com".to_string()],
        subject: "Evil\r\nBcc: victim@example.com".to_string(),
        text_body: Some("body".to_string()),
        ..Default::default()
    };
    let err = send(&evil)
        .await
        .expect_err("a CRLF-injected header must be rejected");
    assert!(
        matches!(err, EmailError::InvalidHeaderValue { .. }),
        "expected InvalidHeaderValue, got {err:?}"
    );
    let got = received.lock().unwrap();
    assert!(
        got.iter().all(|m| !m.subject.contains('\n')),
        "a CRLF-injected message must never reach the backend"
    );
}
