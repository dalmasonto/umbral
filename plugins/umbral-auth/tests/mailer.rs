use std::sync::{Arc, Mutex};
use umbral_auth::mailer::{AuthMailer, OutgoingMail};

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
        subject: "s".into(),
        html: "<b>h</b>".into(),
        text: "t".into(),
    })
    .await
    .unwrap();
    assert_eq!(rec.0.lock().unwrap().len(), 1);
    assert_eq!(rec.0.lock().unwrap()[0].to, "a@b.c");

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
            subject: "".into(),
            html: "".into(),
            text: "".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(*hits.lock().unwrap(), 1);
}
