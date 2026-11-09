//! Request correlation, completion summaries, and opt-in bounded input
//! diagnostics for the outer HTTP boundary.

use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Instant;

use axum::body::{Body, Bytes};
use axum::extract::Request;
use axum::http::{HeaderValue, Method, header};
use axum::middleware::Next;
use axum::response::Response;
use http_body::{Frame, SizeHint};
use tracing::{Instrument, Level, Span};
use uuid::Uuid;

/// Upper bound on the raw bytes captured from a query or request body.
const INPUT_CAPTURE_LIMIT: usize = 8 * 1024;
/// Dedicated target for opt-in input diagnostics.
const INPUT_TARGET: &str = "sunday_slate::http_input";
/// Target for request lifecycle events.
const HTTP_TARGET: &str = "sunday_slate::http";

/// Credential-bearing paths never have queries or bodies captured.
const AUTH_PATHS: [&str; 6] = [
    "/login",
    "/logout",
    "/forgot-password",
    "/reset-password",
    "/setup",
    "/invite",
];

const AUTH_PREFIXES: [&str; 2] = ["/invite/", "/__test/login/"];

fn is_auth_path(path: &str) -> bool {
    AUTH_PATHS.contains(&path) || AUTH_PREFIXES.iter().any(|prefix| path.starts_with(prefix))
}

/// JSON and form bodies are eligible; multipart is logged by its handlers.
fn captures_body(content_type: Option<&HeaderValue>) -> bool {
    let Some(value) = content_type.and_then(|value| value.to_str().ok()) else {
        return false;
    };
    let media_type = value
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    media_type == "application/x-www-form-urlencoded"
        || media_type == "application/json"
        || media_type
            .strip_prefix("application/")
            .is_some_and(|subtype| subtype.ends_with("+json"))
}

fn log_query(query: &str) {
    let bytes = query.as_bytes();
    let truncated = bytes.len() > INPUT_CAPTURE_LIMIT;
    let captured = String::from_utf8_lossy(&bytes[..bytes.len().min(INPUT_CAPTURE_LIMIT)]);
    tracing::debug!(target: INPUT_TARGET, query = ?captured, truncated, "request query");
}

/// Correlation span. Normally INFO, but when the filter suppresses INFO and
/// still admits ERROR, an ERROR span keeps the surviving error events tied to
/// the request. Without it, `x-request-id` would match no log line.
fn request_span(request_id: Uuid, method: &Method, path: &str) -> Span {
    if tracing::enabled!(target: HTTP_TARGET, Level::INFO) {
        tracing::info_span!(
            target: HTTP_TARGET,
            "http_request",
            request_id = %request_id,
            method = %method,
            path = ?path,
        )
    } else if tracing::enabled!(target: HTTP_TARGET, Level::ERROR) {
        tracing::error_span!(
            target: HTTP_TARGET,
            "http_request",
            request_id = %request_id,
            method = %method,
            path = ?path,
        )
    } else {
        Span::none()
    }
}

/// Outer request instrumentation; attach with `.layer`, never `.route_layer`.
pub(crate) async fn request_logging(mut request: Request, next: Next) -> Response {
    let request_id = Uuid::new_v4();
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let span = request_span(request_id, &method, &path);
    let started = Instant::now();

    // No capture storage is allocated unless the target is enabled.
    let capture_input =
        tracing::enabled!(target: INPUT_TARGET, tracing::Level::DEBUG) && !is_auth_path(&path);
    let query = if capture_input {
        request.uri().query().map(str::to_owned)
    } else {
        None
    };
    if capture_input && captures_body(request.headers().get(header::CONTENT_TYPE)) {
        let (parts, body) = request.into_parts();
        request = Request::from_parts(parts, Body::new(InputBody::new(body, span.clone())));
    }

    async move {
        tracing::debug!(target: HTTP_TARGET, "request started");
        if let Some(query) = query {
            log_query(&query);
        }

        let response = next.run(request).await;
        let status = response.status();
        let duration_ms = started.elapsed().as_millis() as u64;

        let (mut parts, body) = response.into_parts();
        // Overwrite: a client-supplied ID is never trusted.
        parts
            .headers
            .insert("x-request-id", HeaderValue::from_str(&request_id.to_string()).expect("uuid is a valid header value"));
        let response = Response::from_parts(
            parts,
            Body::new(ResponseBody {
                inner: body,
                span: Span::current(),
            }),
        );

        if status.is_server_error() {
            tracing::error!(target: HTTP_TARGET, status = status.as_u16(), duration_ms, "request completed");
        } else if status.is_client_error() {
            tracing::warn!(target: HTTP_TARGET, status = status.as_u16(), duration_ms, "request completed");
        } else {
            tracing::info!(target: HTTP_TARGET, status = status.as_u16(), duration_ms, "request completed");
        }
        response
    }
    .instrument(span)
    .await
}

/// Forwards the response body, entering the request span for each poll.
struct ResponseBody {
    inner: Body,
    span: Span,
}

impl http_body::Body for ResponseBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = &mut *self;
        let _entered = this.span.enter();
        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(Some(Err(error))) => {
                tracing::warn!(target: HTTP_TARGET, error = ?error, "response body stream failed");
                Poll::Ready(Some(Err(error)))
            }
            other => other,
        }
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
}

/// Mirrors the request body's prefix; every frame is forwarded untouched.
struct InputBody {
    inner: Body,
    span: Span,
    captured: Vec<u8>,
    truncated: bool,
    complete: bool,
    emitted: bool,
}

impl InputBody {
    fn new(inner: Body, span: Span) -> Self {
        Self {
            inner,
            span,
            captured: Vec::new(),
            truncated: false,
            complete: false,
            emitted: false,
        }
    }

    fn record(&mut self, data: &[u8]) {
        let remaining = INPUT_CAPTURE_LIMIT.saturating_sub(self.captured.len());
        if data.len() > remaining {
            self.captured.extend_from_slice(&data[..remaining]);
            self.truncated = true;
        } else {
            self.captured.extend_from_slice(data);
        }
    }

    /// Emit exactly once; `complete` means downstream saw the whole body.
    fn emit(&mut self, complete: bool) {
        if self.emitted {
            return;
        }
        self.emitted = true;
        self.complete = complete;
        let _entered = self.span.enter();
        let body = String::from_utf8_lossy(&self.captured);
        tracing::debug!(
            target: INPUT_TARGET,
            body = ?body,
            truncated = self.truncated,
            complete = self.complete,
            "request body"
        );
    }
}

impl http_body::Body for InputBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = &mut *self;
        let polled = {
            let _entered = this.span.enter();
            Pin::new(&mut this.inner).poll_frame(cx)
        };
        match polled {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    this.record(data);
                }
                if this.inner.is_end_stream() {
                    this.emit(true);
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(error))) => {
                this.emit(false);
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(None) => {
                this.emit(true);
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
}

impl Drop for InputBody {
    fn drop(&mut self) {
        // Early drop is an incomplete capture, not an empty submission.
        if !self.emitted {
            self.emit(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io::Write;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use std::task::{Context as TaskContext, Poll};

    use axum::Router;
    use axum::body::{Body, Bytes};
    use axum::http::{HeaderMap, HeaderValue, Request, StatusCode, header};
    use axum::response::{IntoResponse, Redirect, Response};
    use axum::routing::{any, get};
    use futures_util::stream;
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    use tracing::instrument::WithSubscriber;
    use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt};

    use super::{INPUT_CAPTURE_LIMIT, request_logging};
    use crate::AppError;

    #[derive(Clone, Default)]
    struct Captured {
        buffer: Arc<Mutex<Vec<u8>>>,
    }

    impl Captured {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.buffer.lock().unwrap()).into_owned()
        }
    }

    impl Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.buffer.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn subscriber(filter: &str, captured: Captured) -> impl tracing::Subscriber + Send + Sync {
        tracing_subscriber::registry()
            .with(EnvFilter::new(filter.to_string()))
            .with(
                fmt::layer()
                    .with_ansi(false)
                    .with_writer(move || captured.clone()),
            )
    }

    fn http_subscriber(captured: Captured) -> impl tracing::Subscriber + Send + Sync {
        // Production-like level; `http_input=off` because EnvFilter matches
        // targets by string prefix.
        subscriber(
            "warn,sunday_slate=info,sunday_slate::http_input=off",
            captured,
        )
    }

    fn input_subscriber(captured: Captured) -> impl tracing::Subscriber + Send + Sync {
        subscriber(
            "warn,sunday_slate=info,sunday_slate::http_input=debug",
            captured,
        )
    }

    async fn probe() -> &'static str {
        "ok"
    }

    async fn echo(body: Body) -> Result<Vec<u8>, AppError> {
        let collected = body
            .collect()
            .await
            .map_err(|error| AppError::Other(anyhow::anyhow!(error)))?;
        Ok(collected.to_bytes().to_vec())
    }

    async fn collect_body(body: Body) -> Result<Response, AppError> {
        let collected = body
            .collect()
            .await
            .map_err(|error| AppError::Other(anyhow::anyhow!(error)))?;
        let trailers = collected.trailers().cloned();
        let mut response = Response::new(Body::from(collected.to_bytes()));
        if let Some(trailers) = trailers {
            response.headers_mut().extend(trailers);
        }
        Ok(response)
    }

    async fn boom() -> Result<&'static str, AppError> {
        Err(AppError::Internal("SENTINEL_CAUSE".into()))
    }

    async fn missing_row() -> Result<&'static str, AppError> {
        Err(AppError::Sqlx(sqlx::Error::RowNotFound))
    }

    async fn redirect() -> Redirect {
        Redirect::temporary("/login")
    }

    async fn events() -> Response {
        let stream = stream::iter(vec![Ok::<_, axum::Error>(Bytes::from_static(
            b"data: one\n\n",
        ))]);
        Response::builder()
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from_stream(stream))
            .unwrap()
    }

    async fn events_error() -> Response {
        let stream = stream::iter(vec![
            Ok(Bytes::from_static(b"data: one\n\n")),
            Err(axum::Error::new(std::io::Error::other(
                "SENTINEL_SSE_ERROR",
            ))),
        ]);
        Response::builder()
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from_stream(stream))
            .unwrap()
    }

    async fn synthetic_failure(_request: Request<Body>, _next: axum::middleware::Next) -> Response {
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    }

    fn router() -> Router {
        Router::new()
            .route("/probe", any(probe))
            .route("/echo", any(echo))
            .route("/collect", any(collect_body))
            .route("/boom", any(boom))
            .route("/missing-row", any(missing_row))
            .route("/redirect", any(redirect))
            .route("/events", get(events))
            .route("/events-error", get(events_error))
            .layer(axum::middleware::from_fn(request_logging))
    }

    fn get_request(uri: &str) -> Request<Body> {
        Request::builder().uri(uri).body(Body::empty()).unwrap()
    }

    async fn response_body(response: Response) -> Vec<u8> {
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec()
    }

    fn request_id(response: &Response) -> uuid::Uuid {
        response
            .headers()
            .get("x-request-id")
            .expect("correlation header")
            .to_str()
            .unwrap()
            .parse()
            .expect("parseable uuid")
    }

    /// Last captured `body=` value from a log line.
    fn last_captured_body(logs: &str) -> Option<String> {
        let marker = "body=\"";
        let start = logs.rfind(marker)? + marker.len();
        let rest = &logs[start..];
        let end = rest.find('"')?;
        Some(rest[..end].to_string())
    }

    fn last_captured_query(logs: &str) -> Option<String> {
        let marker = "query=\"";
        let start = logs.rfind(marker)? + marker.len();
        let rest = &logs[start..];
        let end = rest.find('"')?;
        Some(rest[..end].to_string())
    }

    #[tokio::test]
    async fn correlates_requests_and_overwrites_incoming_ids() {
        let captured = Captured::default();
        async {
            let response = router()
                .oneshot(
                    Request::builder()
                        .uri("/probe?week=9")
                        .header("x-request-id", "untrusted")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let id = request_id(&response);
            assert_ne!(id.to_string(), "untrusted");

            let logs = captured.text();
            assert!(logs.contains(&id.to_string()), "log: {logs}");
            assert!(logs.contains("method=GET"));
            assert!(logs.contains("path=\"/probe\""));
            assert!(logs.contains("status=200"));
            assert!(logs.contains("duration_ms="));
            assert_eq!(logs.matches("request completed").count(), 1);
            assert!(!logs.contains("week=9"), "log: {logs}");
            assert!(!logs.contains("request query"));
        }
        .with_subscriber(http_subscriber(captured.clone()))
        .await;
    }

    #[tokio::test]
    async fn keeps_errors_correlated_when_info_is_filtered_out() {
        let captured = Captured::default();
        async {
            let response = router().oneshot(get_request("/boom")).await.unwrap();
            assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
            let id = request_id(&response);

            let logs = captured.text();
            assert!(logs.contains(&id.to_string()), "log: {logs}");
            assert!(logs.contains("method=GET"), "log: {logs}");
            assert!(logs.contains("path=\"/boom\""), "log: {logs}");
            assert!(logs.contains("SENTINEL_CAUSE"), "log: {logs}");
            assert!(!logs.contains("request started"), "log: {logs}");
        }
        .with_subscriber(subscriber("warn", captured.clone()))
        .await;
    }

    #[tokio::test]
    async fn captures_eligible_inputs_without_altering_them() {
        let captured = Captured::default();
        async {
            let payload = br#"{"probe":"VISIBLE_BODY_SENTINEL"}"#;
            let response = router()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/echo?q=VISIBLE_QUERY_SENTINEL")
                        .header(
                            header::CONTENT_TYPE,
                            "application/vnd.api+json; charset=utf-8",
                        )
                        .body(Body::from(payload.as_slice()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response_body(response).await.as_slice(), payload);

            let logs = captured.text();
            assert!(logs.contains("VISIBLE_QUERY_SENTINEL"), "log: {logs}");
            assert!(logs.contains("VISIBLE_BODY_SENTINEL"), "log: {logs}");
            assert!(logs.contains("truncated=false"));
            assert!(logs.contains("complete=true"));
        }
        .with_subscriber(input_subscriber(captured.clone()))
        .await;
    }

    #[tokio::test]
    async fn forwards_multiframe_bodies_and_logs_only_the_prefix() {
        let captured = Captured::default();
        async {
            let frames = stream::iter(vec![
                Ok::<_, axum::Error>(Bytes::from(vec![b'a'; 6000])),
                Ok(Bytes::from(vec![b'b'; 6000])),
            ]);
            let response = router()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/echo")
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from_stream(frames))
                        .unwrap(),
                )
                .await
                .unwrap();
            let body = response_body(response).await;
            assert_eq!(body.len(), 12_000);
            assert!(body[..6000].iter().all(|byte| *byte == b'a'));
            assert!(body[6000..].iter().all(|byte| *byte == b'b'));

            let logs = captured.text();
            assert!(logs.contains("truncated=true"), "log: {logs}");
            let captured_body = last_captured_body(&logs).unwrap();
            assert_eq!(captured_body.len(), INPUT_CAPTURE_LIMIT);
            assert!(captured_body[..6000].bytes().all(|byte| byte == b'a'));
            assert!(captured_body[6000..].bytes().all(|byte| byte == b'b'));
        }
        .with_subscriber(input_subscriber(captured.clone()))
        .await;
    }

    #[tokio::test]
    async fn bounds_long_queries() {
        let captured = Captured::default();
        async {
            let query = format!("q={}", "z".repeat(9000));
            let response = router()
                .oneshot(get_request(&format!("/probe?{query}")))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);

            let logs = captured.text();
            assert!(logs.contains("truncated=true"), "log: {logs}");
            assert_eq!(
                last_captured_query(&logs).unwrap().len(),
                INPUT_CAPTURE_LIMIT
            );
        }
        .with_subscriber(input_subscriber(captured.clone()))
        .await;
    }

    #[tokio::test]
    async fn forwards_frames_trailers_errors_and_incomplete_bodies() {
        let captured = Captured::default();
        async {
            let mut trailers = HeaderMap::new();
            trailers.insert("x-checksum", HeaderValue::from_static("ok"));
            let body = Body::new(Frames(VecDeque::from(vec![
                Ok(http_body::Frame::data(Bytes::from_static(b"TRAILER_DATA"))),
                Ok(http_body::Frame::trailers(trailers)),
            ])));
            let response = router()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/collect")
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(body)
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.headers()["x-checksum"].to_str().unwrap(), "ok");
            assert_eq!(response_body(response).await.as_slice(), b"TRAILER_DATA");
            let logs = captured.text();
            assert_eq!(last_captured_body(&logs).as_deref(), Some("TRAILER_DATA"));
            assert!(logs.contains("complete=true"));

            let body = Body::new(Frames(VecDeque::from(vec![
                Ok(http_body::Frame::data(Bytes::from_static(b"PARTIAL_DATA"))),
                Err(axum::Error::new(std::io::Error::other(
                    "SENTINEL_INPUT_ERROR",
                ))),
            ])));
            let response = router()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/echo")
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(body)
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
            let logs = captured.text();
            assert!(logs.contains("SENTINEL_INPUT_ERROR"), "log: {logs}");
            assert_eq!(last_captured_body(&logs).as_deref(), Some("PARTIAL_DATA"));
            assert!(logs.contains("complete=false"));

            let response = router()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/probe")
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from("UNUSED_DATA"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let logs = captured.text();
            assert_eq!(last_captured_body(&logs).as_deref(), Some(""));
            assert!(logs.contains("complete=false"));

            let response = router()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/echo")
                        .header(header::CONTENT_TYPE, "multipart/form-data; boundary=xyz")
                        .body(Body::from("MULTIPART_SENTINEL"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response_body(response).await.as_slice(),
                b"MULTIPART_SENTINEL"
            );
            assert!(!captured.text().contains("MULTIPART_SENTINEL"));
        }
        .with_subscriber(input_subscriber(captured.clone()))
        .await;
    }

    #[tokio::test]
    async fn lossily_logs_invalid_utf8_without_altering_it() {
        let captured = Captured::default();
        async {
            let payload = vec![0xff, 0xfe, b'A'];
            let response = router()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/echo")
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(payload.clone()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response_body(response).await.as_slice(), payload);

            let logs = captured.text();
            assert!(
                last_captured_body(&logs).unwrap().contains('\u{fffd}'),
                "log: {logs}"
            );
        }
        .with_subscriber(input_subscriber(captured.clone()))
        .await;
    }

    #[tokio::test]
    async fn escapes_newlines_in_captured_input() {
        let captured = Captured::default();
        async {
            let payload = "line1\nFAKE 2026-01-01T00:00:00Z ERROR entry";
            let response = router()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/echo")
                        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                        .body(Body::from(payload))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response_body(response).await.as_slice(), payload.as_bytes());

            let logs = captured.text();
            let captured_body = last_captured_body(&logs).unwrap();
            // Debug-escaped: a newline cannot split the log line.
            assert!(captured_body.contains("\\n"), "log: {logs}");
            assert!(!captured_body.contains('\n'), "log: {logs}");
        }
        .with_subscriber(input_subscriber(captured.clone()))
        .await;
    }

    #[tokio::test]
    async fn never_captures_credential_paths() {
        let captured = Captured::default();
        async {
            let excluded = [
                "/login",
                "/logout",
                "/forgot-password",
                "/reset-password",
                "/setup",
                "/invite",
                "/invite/create-user",
                "/__test/login/1",
            ];
            for (index, path) in excluded.iter().enumerate() {
                let sentinel = format!("AUTH_SENTINEL_{index}");
                let response = router()
                    .oneshot(
                        Request::builder()
                            .method("POST")
                            .uri(format!("{path}?secret={sentinel}"))
                            .header(header::CONTENT_TYPE, "application/json")
                            .header(header::COOKIE, format!("session={sentinel}"))
                            .header(header::AUTHORIZATION, format!("Bearer {sentinel}"))
                            .body(Body::from(format!("{{\"password\":\"{sentinel}\"}}")))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                drop(response_body(response).await);
            }
            let logs = captured.text();
            assert!(!logs.contains("AUTH_SENTINEL"), "log: {logs}");

            let response = router()
                .oneshot(get_request("/invites?secret=ELIGIBLE_SENTINEL"))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            assert!(captured.text().contains("ELIGIBLE_SENTINEL"));
        }
        .with_subscriber(input_subscriber(captured.clone()))
        .await;
    }

    #[tokio::test]
    async fn error_responses_keep_correlation_and_hide_details() {
        let captured = Captured::default();
        async {
            let response = router().oneshot(get_request("/boom")).await.unwrap();
            assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
            let id = request_id(&response);
            assert_eq!(
                response_body(response).await.as_slice(),
                b"Internal server error"
            );
            let logs = captured.text();
            assert!(logs.contains(&id.to_string()), "log: {logs}");
            assert!(logs.contains("SENTINEL_CAUSE"), "log: {logs}");
            assert!(logs.contains("status=500"));

            let response = router().oneshot(get_request("/missing-row")).await.unwrap();
            // Message stays the existing client-level text.
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            let _ = request_id(&response);

            let response = router().oneshot(get_request("/redirect")).await.unwrap();
            assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
            assert_eq!(
                response.headers()[header::LOCATION].to_str().unwrap(),
                "/login"
            );
            let _ = request_id(&response);

            let response = router()
                .oneshot(get_request("/unknown-route"))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            let _ = request_id(&response);

            let synthetic = Router::new()
                .route("/synthetic", get(|| async { "unreachable" }))
                .layer(axum::middleware::from_fn(synthetic_failure))
                .layer(axum::middleware::from_fn(request_logging));
            let response = synthetic.oneshot(get_request("/synthetic")).await.unwrap();
            assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
            let _ = request_id(&response);
        }
        .with_subscriber(http_subscriber(captured.clone()))
        .await;
    }

    #[tokio::test]
    async fn sse_summarizes_on_headers_and_forwards_stream_errors() {
        let captured = Captured::default();
        async {
            let response = router().oneshot(get_request("/events")).await.unwrap();
            let logs = captured.text();
            assert_eq!(logs.matches("request completed").count(), 1);
            assert!(logs.contains("status=200"));
            assert!(!logs.contains("response body stream failed"));

            let body = response_body(response).await;
            assert_eq!(body.as_slice(), b"data: one\n\n");
            assert!(!captured.text().contains("response body stream failed"));

            let response = router()
                .oneshot(get_request("/events-error"))
                .await
                .unwrap();
            let collected = response.into_body().collect().await;
            assert!(collected.is_err());
            let logs = captured.text();
            assert!(logs.contains("response body stream failed"), "log: {logs}");
            assert!(logs.contains("SENTINEL_SSE_ERROR"), "log: {logs}");

            let response = router().oneshot(get_request("/events")).await.unwrap();
            drop(response);
            assert_eq!(
                captured
                    .text()
                    .matches("response body stream failed")
                    .count(),
                1
            );
        }
        .with_subscriber(http_subscriber(captured.clone()))
        .await;
    }

    /// Minimal body yielding scripted frames, including errors and trailers.
    struct Frames(VecDeque<Result<http_body::Frame<Bytes>, axum::Error>>);

    impl http_body::Body for Frames {
        type Data = Bytes;
        type Error = axum::Error;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            _cx: &mut TaskContext<'_>,
        ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
            Poll::Ready(self.0.pop_front())
        }
    }
}
