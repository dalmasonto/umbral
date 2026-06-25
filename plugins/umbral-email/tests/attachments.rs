//! End-to-end tests for [`umbral_email::EmailMessage::attach`].
//!
//! Drives `compose` directly (the public bridge into lettre) and
//! inspects the resulting RFC 822 / MIME bytes via
//! `Message::formatted()`. Pins the on-wire shape: content type,
//! attachment filename, base64 body, multipart boundaries.

use umbral_email::{Attachment, EmailMessage, compose};

/// Render an `EmailMessage` to the wire bytes and return them as a
/// string. The wire format is ASCII for headers and base64 for binary
/// payloads, so lossy UTF-8 is safe for assertion text.
fn wire(message: &EmailMessage) -> String {
    let composed = compose("Acme <noreply@acme.test>", message).expect("compose");
    String::from_utf8_lossy(&composed.formatted()).into_owned()
}

// =====================================================================
// No attachments — backward-compat path. Top-level Content-Type is
// NOT multipart/mixed; the existing single/alternative shape holds.
// =====================================================================

#[test]
fn text_only_without_attachments_stays_single_part() {
    let msg = EmailMessage::new("Hi", vec!["a@b.test".into()]).text_body("hello");
    let bytes = wire(&msg);
    assert!(
        !bytes.to_ascii_lowercase().contains("multipart/mixed"),
        "no attachments → no multipart/mixed wrapper; got:\n{bytes}"
    );
    assert!(
        bytes.to_ascii_lowercase().contains("text/plain"),
        "text body present; got:\n{bytes}"
    );
}

#[test]
fn both_bodies_without_attachments_stay_alternative() {
    let msg = EmailMessage::new("Hi", vec!["a@b.test".into()])
        .text_body("hello text")
        .html_body("<p>hello html</p>");
    let bytes = wire(&msg);
    let lower = bytes.to_ascii_lowercase();
    assert!(
        lower.contains("multipart/alternative"),
        "both bodies → alternative; got:\n{bytes}"
    );
    assert!(
        !lower.contains("multipart/mixed"),
        "no attachments → no mixed wrapper; got:\n{bytes}"
    );
}

// =====================================================================
// Single attachment — top-level becomes multipart/mixed.
// =====================================================================

#[test]
fn single_attachment_wraps_body_in_multipart_mixed() {
    let msg = EmailMessage::new("Invoice", vec!["a@b.test".into()])
        .text_body("Your invoice is attached.")
        .attach("invoice.pdf", "application/pdf", b"%PDF-1.4 fake".to_vec());
    let bytes = wire(&msg);
    let lower = bytes.to_ascii_lowercase();
    assert!(
        lower.contains("multipart/mixed"),
        "single attachment should produce multipart/mixed; got:\n{bytes}"
    );
    assert!(
        lower.contains("application/pdf"),
        "attachment content-type missing; got:\n{bytes}"
    );
    assert!(
        bytes.contains("invoice.pdf"),
        "attachment filename missing; got:\n{bytes}"
    );
}

#[test]
fn binary_attachment_body_appears_base64_encoded() {
    // Pure-ASCII payloads stay as 7bit; lettre only base64s when the
    // body contains bytes that 7bit can't carry. Use a binary
    // payload (high bit, NUL) to force base64 transfer-encoding.
    let payload: Vec<u8> = vec![0xFF, 0x00, 0xAB, 0xCD, 0xEF, 0xFE];
    let msg = EmailMessage::new("Data", vec!["a@b.test".into()])
        .text_body("see attachment")
        .attach("data.bin", "application/octet-stream", payload.clone());
    let bytes = wire(&msg);
    // base64(0xFF 0x00 0xAB 0xCD 0xEF 0xFE) = "/wCrze/+".
    assert!(
        bytes.contains("/wCrze/+"),
        "expected base64 of the binary payload; got:\n{bytes}"
    );
    assert!(
        bytes.to_ascii_lowercase().contains("base64"),
        "Content-Transfer-Encoding: base64 header should be set; got:\n{bytes}"
    );
}

#[test]
fn attachment_carries_content_disposition_attachment() {
    let msg = EmailMessage::new("Hi", vec!["a@b.test".into()])
        .text_body("see attached")
        .attach("readme.txt", "text/plain", b"file contents".to_vec());
    let bytes = wire(&msg);
    let lower = bytes.to_ascii_lowercase();
    assert!(
        lower.contains("content-disposition: attachment"),
        "Content-Disposition header missing; got:\n{bytes}"
    );
}

// =====================================================================
// Body shapes nested inside multipart/mixed.
// =====================================================================

#[test]
fn text_and_html_with_attachment_nests_alternative_inside_mixed() {
    let msg = EmailMessage::new("Hi", vec!["a@b.test".into()])
        .text_body("plain")
        .html_body("<p>html</p>")
        .attach("note.txt", "text/plain", b"note".to_vec());
    let bytes = wire(&msg);
    let lower = bytes.to_ascii_lowercase();
    assert!(
        lower.contains("multipart/mixed"),
        "outer mixed; got:\n{bytes}"
    );
    assert!(
        lower.contains("multipart/alternative"),
        "inner alternative for the two body forms; got:\n{bytes}"
    );
}

#[test]
fn html_only_body_with_attachment_renders_html_part_plus_attachment() {
    let msg = EmailMessage::new("Hi", vec!["a@b.test".into()])
        .html_body("<h1>html only</h1>")
        .attach("note.txt", "text/plain", b"note".to_vec());
    let bytes = wire(&msg);
    let lower = bytes.to_ascii_lowercase();
    assert!(lower.contains("multipart/mixed"), "outer mixed");
    assert!(
        lower.contains("text/html"),
        "html body part present; got:\n{bytes}"
    );
}

// =====================================================================
// Multiple attachments.
// =====================================================================

#[test]
fn multiple_attachments_all_appear_with_their_filenames() {
    let msg = EmailMessage::new("Reports", vec!["a@b.test".into()])
        .text_body("Three reports attached.")
        .attach("q1.pdf", "application/pdf", b"q1 fake".to_vec())
        .attach("q2.pdf", "application/pdf", b"q2 fake".to_vec())
        .attach("q3.pdf", "application/pdf", b"q3 fake".to_vec());
    let bytes = wire(&msg);
    assert!(bytes.contains("q1.pdf"));
    assert!(bytes.contains("q2.pdf"));
    assert!(bytes.contains("q3.pdf"));
}

// =====================================================================
// Builder field — `attachments` Vec is populated correctly.
// =====================================================================

#[test]
fn attach_pushes_onto_attachments_vec_in_order() {
    let msg = EmailMessage::new("Hi", vec!["a@b.test".into()])
        .attach("a.txt", "text/plain", b"A".to_vec())
        .attach("b.txt", "text/plain", b"B".to_vec());
    assert_eq!(msg.attachments.len(), 2);
    assert_eq!(msg.attachments[0].filename, "a.txt");
    assert_eq!(msg.attachments[1].filename, "b.txt");
}

#[test]
fn attachment_struct_can_be_constructed_directly() {
    let att = Attachment::new("file.bin", "application/octet-stream", vec![1, 2, 3]);
    assert_eq!(att.filename, "file.bin");
    assert_eq!(att.content_type, "application/octet-stream");
    assert_eq!(att.data, vec![1, 2, 3]);
}

// =====================================================================
// Bad content type → InvalidAttachmentContentType, not panic.
// =====================================================================

#[test]
fn invalid_content_type_returns_named_error() {
    let msg = EmailMessage::new("Hi", vec!["a@b.test".into()])
        .text_body("body")
        .attach("file.bin", "not a valid mime type", b"data".to_vec());
    let err = compose("a@b.test", &msg).expect_err("should fail");
    let text = err.to_string();
    assert!(
        text.contains("file.bin"),
        "error should name the offending attachment; got: {text}"
    );
    assert!(
        text.contains("invalid content type"),
        "error should mention invalid content type; got: {text}"
    );
}
