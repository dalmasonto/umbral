use std::sync::{Arc, Mutex};
use umbral_auth::mailer::{AuthMailer, MailKind, OutgoingMail};

#[derive(Default, Clone)]
struct Recorder(Arc<Mutex<Vec<OutgoingMail>>>);
#[async_trait::async_trait]
impl AuthMailer for Recorder {
    async fn send(&self, mail: OutgoingMail) -> Result<(), umbral_auth::mailer::AuthMailError> {
        self.0.lock().unwrap().push(mail);
        Ok(())
    }
}

#[tokio::test]
async fn recorder_mailer_captures_and_closure_impl_works() {
    let rec = Recorder::default();
    rec.send(OutgoingMail {
        to: "a@b.c".into(),
        username: "alice".into(),
        kind: MailKind::EmailVerification {
            code: "483920".into(),
        },
        subject: "s".into(),
        html: "<b>h</b>".into(),
        text: "t".into(),
    })
    .await
    .unwrap();
    assert_eq!(rec.0.lock().unwrap().len(), 1);
    assert_eq!(rec.0.lock().unwrap()[0].to, "a@b.c");
    // The semantic kind + its params reach the mailer unchanged.
    assert!(matches!(
        &rec.0.lock().unwrap()[0].kind,
        MailKind::EmailVerification { code } if code == "483920"
    ));

    // Closure blanket impl: a plain async closure is an AuthMailer.
    let hits = Arc::new(Mutex::new(0));
    let h2 = hits.clone();
    let closure = move |_m: OutgoingMail| {
        let h = h2.clone();
        async move {
            *h.lock().unwrap() += 1;
            Ok(())
        }
    };
    AuthMailer::send(
        &closure,
        OutgoingMail {
            to: "x".into(),
            username: "x".into(),
            kind: MailKind::PasswordReset {
                reset_url: "/auth/reset?token=t".into(),
            },
            subject: "".into(),
            html: "".into(),
            text: "".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(*hits.lock().unwrap(), 1);
}

/// Audit plugin-auth #7: outside Dev/Test the ConsoleMailer must NOT print the
/// secret-bearing body (verification code / reset link). In Dev/Test it still
/// prints the full body for developer visibility.
#[test]
fn console_output_suppresses_secret_body_in_prod() {
    use umbral_auth::mailer::console_output;

    let mail = OutgoingMail {
        to: "victim@example.com".into(),
        username: "victim".into(),
        kind: MailKind::PasswordReset {
            reset_url: "https://app/auth/reset?token=umbral_SECRET123".into(),
        },
        subject: "Reset your password".into(),
        html: "<a href=\"https://app/auth/reset?token=umbral_SECRET123\">reset</a>".into(),
        text: "Reset: https://app/auth/reset?token=umbral_SECRET123".into(),
    };

    // Prod: the secret must never appear; recipient + subject still shown.
    let prod = console_output(&mail, true);
    assert!(
        !prod.contains("umbral_SECRET123"),
        "prod console output must not leak the reset token; got {prod}"
    );
    assert!(
        !prod.contains(&mail.text),
        "prod console output must not print the rendered body; got {prod}"
    );
    assert!(prod.contains("victim@example.com"));
    assert!(prod.contains("SUPPRESSED"));

    // Dev/Test: full body (including the secret) is printed for the developer.
    let dev = console_output(&mail, false);
    assert!(
        dev.contains("umbral_SECRET123"),
        "dev console output should print the body so a developer can copy the link; got {dev}"
    );
}

#[tokio::test]
async fn console_mailer_send_does_not_panic_without_settings() {
    use umbral_auth::mailer::{AuthMailer, ConsoleMailer, MailKind, OutgoingMail};
    ConsoleMailer
        .send(OutgoingMail {
            to: "dev@test".into(),
            username: "dev".into(),
            kind: MailKind::EmailVerification {
                code: "000000".into(),
            },
            subject: "x".into(),
            html: String::new(),
            text: String::new(),
        })
        .await
        .unwrap();
}
