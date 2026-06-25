//! Streaming HTTP response bodies (feature #70).
//!
//! A normal umbral handler returns a fully-buffered `String` / `Html` /
//! `Json`. That's fine for a page, but a 200 MB CSV export or a file
//! download shouldn't sit in memory all at once. [`StreamingResponse`]
//! sends the body chunk-by-chunk from an async [`Stream`], so memory stays
//! flat regardless of payload size and the client starts receiving bytes
//! before the last row is generated.
//!
//! Pairs with [`AppBuilder::compression`](crate::app::AppBuilder::compression):
//! a streamed body is gzip/brotli-compressed on the fly by the same layer,
//! so `?format=csv` over a million rows streams *and* compresses without
//! buffering.

use axum::body::{Body, Bytes};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::{Stream, StreamExt, TryStream};

/// A response whose body is produced incrementally from an async stream.
///
/// Build it from a stream, set the content type and (optionally) a
/// download disposition, then return it from a handler — it implements
/// [`IntoResponse`].
///
/// ```ignore
/// use umbral::web::StreamingResponse;
/// use futures_util::stream;
///
/// async fn export() -> StreamingResponse {
///     // each item is a chunk of the body; generate them lazily
///     let rows = stream::iter((0..1_000_000).map(|i| format!("row {i}\n")));
///     StreamingResponse::from_chunks(rows)
///         .content_type("text/csv; charset=utf-8")
///         .attachment("export.csv")
/// }
/// ```
pub struct StreamingResponse {
    body: Body,
    content_type: String,
    content_disposition: Option<String>,
    status: StatusCode,
}

impl StreamingResponse {
    /// Build from a **fallible** byte stream: each item is
    /// `Result<impl Into<Bytes>, impl Into<BoxError>>`. Use this when
    /// producing a chunk can fail (a DB row read, a file `read` call). A
    /// stream error aborts the response mid-flight — the client sees a
    /// truncated body, the honest outcome for an already-started stream
    /// (the status line and headers have already been sent).
    pub fn new<S>(stream: S) -> Self
    where
        S: TryStream + Send + 'static,
        S::Ok: Into<Bytes>,
        S::Error: Into<axum::BoxError>,
    {
        Self {
            body: Body::from_stream(stream),
            content_type: "application/octet-stream".to_string(),
            content_disposition: None,
            status: StatusCode::OK,
        }
    }

    /// Build from an **infallible** chunk stream: each item is
    /// `impl Into<Bytes>` (`String`, `Bytes`, `Vec<u8>`, `&'static str`).
    /// The common "generate rows, never error" case.
    pub fn from_chunks<S, T>(stream: S) -> Self
    where
        S: Stream<Item = T> + Send + 'static,
        T: Into<Bytes>,
    {
        let try_stream = stream.map(|chunk| Ok::<Bytes, std::convert::Infallible>(chunk.into()));
        Self::new(try_stream)
    }

    /// Set the `Content-Type`. Defaults to `application/octet-stream`.
    pub fn content_type(mut self, content_type: impl Into<String>) -> Self {
        self.content_type = content_type.into();
        self
    }

    /// Mark the body as a file download:
    /// `Content-Disposition: attachment; filename="<name>"`. The browser
    /// offers a save dialog instead of rendering the body.
    pub fn attachment(mut self, filename: impl Into<String>) -> Self {
        self.content_disposition = Some(format!(
            "attachment; filename=\"{}\"",
            sanitize_filename(&filename.into())
        ));
        self
    }

    /// `Content-Disposition: inline; filename="<name>"` — display in the
    /// browser when it can, with a suggested name for "save as".
    pub fn inline(mut self, filename: impl Into<String>) -> Self {
        self.content_disposition = Some(format!(
            "inline; filename=\"{}\"",
            sanitize_filename(&filename.into())
        ));
        self
    }

    /// Override the status code (defaults to `200 OK`).
    pub fn status(mut self, status: StatusCode) -> Self {
        self.status = status;
        self
    }
}

impl IntoResponse for StreamingResponse {
    fn into_response(self) -> Response {
        let mut builder = Response::builder()
            .status(self.status)
            .header(header::CONTENT_TYPE, self.content_type);
        if let Some(cd) = self.content_disposition {
            builder = builder.header(header::CONTENT_DISPOSITION, cd);
        }
        builder
            .body(self.body)
            .expect("content-type / content-disposition are always valid header values")
    }
}

/// Drop CR / LF / `"` from a filename so it can't inject extra response
/// headers or break out of the quoted `Content-Disposition` value.
fn sanitize_filename(name: &str) -> String {
    name.chars()
        .filter(|c| !matches!(c, '\r' | '\n' | '"'))
        .collect()
}
