//! Feature #70 — `StreamingResponse`: an HTTP body produced incrementally
//! from an async stream, so a large export isn't buffered in memory.
//! These exercise `into_response()` directly (no `App::build`, so several
//! `#[tokio::test]`s are fine).

use axum::body::Bytes;
use axum::response::IntoResponse;
use futures_util::stream;
use http_body_util::BodyExt;
use umbra::web::StreamingResponse;

#[tokio::test]
async fn from_chunks_streams_concatenated_body_with_headers() {
    let chunks = stream::iter(vec![
        "a,b,c\n".to_string(),
        "1,2,3\n".to_string(),
        "4,5,6\n".to_string(),
    ]);
    let resp = StreamingResponse::from_chunks(chunks)
        .content_type("text/csv; charset=utf-8")
        .attachment("export.csv")
        .into_response();

    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "text/csv; charset=utf-8"
    );
    assert_eq!(
        resp.headers().get("content-disposition").unwrap(),
        "attachment; filename=\"export.csv\""
    );

    // The body is the chunks concatenated, in order.
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"a,b,c\n1,2,3\n4,5,6\n");
}

#[tokio::test]
async fn new_propagates_a_mid_stream_error_as_truncation() {
    // Two good chunks, then the producer fails.
    let chunks = stream::iter(vec![
        Ok::<Bytes, std::io::Error>(Bytes::from_static(b"part1")),
        Ok(Bytes::from_static(b"part2")),
        Err(std::io::Error::other("boom")),
    ]);
    let resp = StreamingResponse::new(chunks).into_response();

    // Headers/status were already sent; the error surfaces when the body
    // is consumed (a truncated download, the honest outcome).
    let collected = resp.into_body().collect().await;
    assert!(
        collected.is_err(),
        "a mid-stream error surfaces while collecting the body"
    );
}

#[tokio::test]
async fn attachment_filename_is_sanitized_against_header_injection() {
    let resp = StreamingResponse::from_chunks(stream::iter(vec!["x".to_string()]))
        .attachment("evil\r\nSet-Cookie: pwned.csv")
        .into_response();
    let cd = resp
        .headers()
        .get("content-disposition")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        !cd.contains('\r') && !cd.contains('\n'),
        "CR/LF stripped from filename: {cd}"
    );
    assert_eq!(cd, "attachment; filename=\"evilSet-Cookie: pwned.csv\"");
}

#[tokio::test]
async fn defaults_to_octet_stream_with_no_disposition() {
    let resp = StreamingResponse::from_chunks(stream::iter(vec!["x".to_string()])).into_response();
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "application/octet-stream"
    );
    assert!(resp.headers().get("content-disposition").is_none());
}
