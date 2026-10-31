//! The axum side: one catch-all route feeding the core dispatcher.
//!
//! `fqapi_core::dispatch` already owns path matching, parameter parsing and the
//! JSON envelope, so this module is only a translator between `http::Request`
//! and `dispatch::Request` - plus the pieces a public listener needs that the
//! loopback adapter deliberately lacks (auth layer, concurrency gate, deadline,
//! CORS, streaming file bodies).

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{middleware, Router};
use fqapi_core::dispatch::{self, ResponseBody};
use fqapi_core::endpoints::Server;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;
use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::auth;
use crate::health;
use crate::history::History;
use crate::tokens::TokenStore;

/// Largest request body accepted. No route reads a body bigger than the
/// 16 KiB account-capture limit (everything else takes its arguments from the
/// query string), so a megabyte is already generous and keeps the memory
/// ceiling at `max_concurrency * 1 MiB`.
pub const MAX_BODY: usize = 1024 * 1024;

#[derive(Clone)]
pub struct AppState {
    pub server: Arc<Server>,
    pub tokens: Arc<TokenStore>,
    /// Recent requests, for the admin page. Bounded and in memory.
    pub history: Arc<History>,
    /// Admin page sessions. Die with the process.
    pub sessions: crate::admin::Sessions,
    pub anonymous_read: bool,
    /// Bounds in-flight upstream work. Queueing, not rejecting: a full pool is
    /// a throughput problem, not a client error.
    pub semaphore: Arc<tokio::sync::Semaphore>,
    pub timeout: Duration,
    pub cors_origins: Vec<String>,
}

impl AppState {
    pub fn from_config(server: Arc<Server>, cfg: &crate::config::Config) -> Self {
        AppState {
            server,
            // CLI/environment tokens seed the store; the admin page can add
            // more, and those persist next to the device pool.
            tokens: Arc::new(TokenStore::load(&cfg.data_dir, cfg.tokens.clone())),
            history: Arc::new(History::new()),
            sessions: crate::admin::sessions(),
            anonymous_read: cfg.anonymous_read,
            semaphore: Arc::new(tokio::sync::Semaphore::new(cfg.max_concurrency)),
            timeout: cfg.timeout,
            cors_origins: cfg.cors_origins.clone(),
        }
    }
}

/// Builds the router. `healthz`/`readyz` sit outside the authenticated layer:
/// a probe has no token, and neither route reveals content.
pub fn router(state: AppState) -> Result<Router, String> {
    let api = Router::new()
        .fallback(dispatch_handler)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_token,
        ));

    let mut app: Router<AppState> = Router::new()
        .route("/healthz", get(health::healthz))
        .route("/readyz", get(health::readyz))
        .merge(api)
        // The admin page authenticates with its own session cookie, so it
        // cannot sit behind the bearer-token layer above.
        .merge(crate::admin::router())
        .layer(DefaultBodyLimit::max(MAX_BODY))
        // Outermost of the inner layers, so a rejected request (401, 413) is
        // still recorded - those are exactly the lines an operator looks for.
        .layer(middleware::from_fn_with_state(state.clone(), log_access));

    if let Some(cors) = cors_layer(&state.cors_origins)? {
        app = app.layer(cors);
    }
    Ok(app.with_state(state))
}

/// One line per request, on stdout, plus a row in the admin page's history.
///
/// Deliberately not routed through `log`: the file sink rotates at 1 MiB with a
/// single backup, so access lines would evict the diagnostics it exists to
/// keep. The query string is never logged - media URLs carry the token in it.
///
/// `/admin` requests are logged but not recorded: the page polls its own state
/// every few seconds, which would bury the API traffic an operator is looking
/// for.
async fn log_access(
    State(state): State<AppState>,
    request: Request,
    next: middleware::Next,
) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let started = Instant::now();

    let response = next.run(request).await;

    let millis = started.elapsed().as_millis() as u64;
    println!(
        "[I] {} access: {method} {path} {} {}ms",
        iso_utc_now(),
        response.status().as_u16(),
        millis
    );
    if !path.starts_with("/admin") {
        state
            .history
            .record(method.as_str(), &path, response.status().as_u16(), millis);
    }
    response
}

fn iso_utc_now() -> String {
    fqapi_core::timeutil::format_unix(fqapi_core::timeutil::now_secs()).replacen(' ', "T", 1)
}

/// The cached-media namespace, served by `fqapi_core::static_files`.
///
/// Matched exactly: `/srcx` is not media, and neither is `/api/v1/src/...`.
pub(crate) fn is_media_path(path: &str) -> bool {
    path == "/src" || path.starts_with("/src/")
}

async fn dispatch_handler(State(state): State<AppState>, request: Request) -> Response {
    let permit = match state.semaphore.clone().acquire_owned().await {
        Ok(permit) => permit,
        Err(_) => return json_error(StatusCode::SERVICE_UNAVAILABLE, "server is shutting down"),
    };

    let (parts, body) = request.into_parts();
    let body = match axum::body::to_bytes(body, MAX_BODY).await {
        Ok(bytes) => bytes.to_vec(),
        Err(_) => return json_error(StatusCode::PAYLOAD_TOO_LARGE, "request body too large"),
    };

    let core_request = dispatch::Request {
        method: parts.method.to_string(),
        path: parts.uri.path().to_string(),
        query: parts.uri.query().unwrap_or("").to_string(),
        body,
        headers: parts
            .headers
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    String::from_utf8_lossy(value.as_bytes()).into_owned(),
                )
            })
            .collect(),
    };
    let media = is_media_path(&core_request.path);

    // Dropping this future is the cancellation. hyper drops the handler future
    // when the client disconnects, which tears down the awaiting upstream
    // request and any retry backoff - the same guarantee the id-keyed
    // `runtime::request` gives the Flutter bridge, without needing the id.
    let outcome = tokio::time::timeout(
        state.timeout,
        dispatch::dispatch(&state.server, &core_request),
    )
    .await;
    drop(permit);

    let mut response = match outcome {
        Ok(response) => to_http(response).await,
        Err(_) => json_error(StatusCode::GATEWAY_TIMEOUT, "timeout"),
    };
    if media {
        // Media accepts its credential in the query string (an `<img>` tag
        // cannot send a header), so keep that URL out of referrers and out of
        // shared caches.
        response.headers_mut().insert(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        );
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, max-age=3600"),
        );
    }
    response
}

/// Converts a core response, streaming file bodies instead of buffering them.
async fn to_http(response: dispatch::Response) -> Response {
    let status = StatusCode::from_u16(response.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, response.content_type.as_str());
    for (name, value) in &response.headers {
        builder = builder.header(name.as_str(), value.as_str());
    }

    match response.body {
        ResponseBody::Bytes(bytes) => builder
            .body(Body::from(bytes))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        ResponseBody::File {
            path,
            offset,
            length,
            ..
        } => match stream_file(&path, offset, length).await {
            Ok(body) => builder
                .body(body)
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
            Err(_) => json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to read the cached resource",
            ),
        },
        ResponseBody::FileDecrypted {
            path,
            offset,
            length,
            index,
            key,
            ..
        } => match stream_decrypted_file(&path, offset, length, index, &key).await {
            Ok(body) => builder
                .body(body)
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
            Err(_) => json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to read the cached resource",
            ),
        },
    }
}

async fn stream_file(path: &Path, offset: u64, length: u64) -> Result<Body, String> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    if offset > 0 {
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(Body::from_stream(ReaderStream::with_capacity(
        file.take(length),
        64 * 1024,
    )))
}

/// Streams a range of a CENC-encrypted cache file, decrypting each chunk at
/// its absolute file offset as it passes (P1-7). Chunks arrive sequentially,
/// and the transform is position-exact, so chunk boundaries do not matter.
async fn stream_decrypted_file(
    path: &Path,
    offset: u64,
    length: u64,
    index: Arc<fqapi_core::endpoints::video::CencIndex>,
    key: &[u8],
) -> Result<Body, String> {
    use futures::StreamExt;
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    if offset > 0 {
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|e| e.to_string())?;
    }
    let limited = file.take(length);
    let key = key.to_vec();
    let mut pos = offset;
    let stream = ReaderStream::with_capacity(limited, 64 * 1024).map(move |chunk| {
        chunk.map(|bytes| {
            let mut buf = bytes.to_vec();
            // The key was validated when the response was built; a failure
            // here can only come from a torn read, and passing the chunk
            // through untransformed is no worse than the I/O error path.
            let _ = fqapi_core::endpoints::video::apply_cenc_index(&mut buf, pos, &index, &key);
            pos += buf.len() as u64;
            axum::body::Bytes::from(buf)
        })
    });
    Ok(Body::from_stream(stream))
}

/// The error envelope every route already uses, so a client parses one shape.
pub(crate) fn json_error(status: StatusCode, message: &str) -> Response {
    let body = serde_json::json!({ "success": false, "error": message });
    (
        status,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        body.to_string(),
    )
        .into_response()
}

fn cors_layer(origins: &[String]) -> Result<Option<CorsLayer>, String> {
    if origins.is_empty() {
        return Ok(None);
    }
    let mut allowed = Vec::with_capacity(origins.len());
    for origin in origins {
        // A header value alone is too permissive: `app.example` (no scheme) is
        // a valid header value that simply never matches, which would leave a
        // web client failing CORS with no hint that the config was wrong.
        if !(origin.starts_with("http://") || origin.starts_with("https://")) {
            return Err(format!(
                "CORS origin must start with http:// or https://: {origin}"
            ));
        }
        allowed.push(
            HeaderValue::from_str(origin).map_err(|_| format!("invalid CORS origin: {origin}"))?,
        );
    }
    Ok(Some(
        CorsLayer::new()
            .allow_origin(AllowOrigin::list(allowed))
            .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
            .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_origins_means_no_cors_layer() {
        assert!(cors_layer(&[]).expect("empty").is_none());
    }

    #[test]
    fn an_origin_without_a_scheme_is_rejected() {
        // This is a legal header value, so only the scheme check catches it -
        // and without that check the server would start with a CORS rule that
        // can never match.
        assert!(cors_layer(&["app.example".to_string()]).is_err());
    }

    #[test]
    fn an_origin_that_is_not_a_header_value_is_rejected() {
        assert!(cors_layer(&["https://a.example\nX-Evil: 1".to_string()]).is_err());
    }

    #[test]
    fn valid_origins_build_a_layer() {
        assert!(cors_layer(&["https://app.example".to_string()])
            .expect("valid")
            .is_some());
    }
}
