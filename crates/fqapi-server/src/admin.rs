//! Admin page and its JSON API.
//!
//! Scope: what an operator actually needs on a self-hosted box - is it alive,
//! which devices does it hold, which tokens exist, what has it been asked to
//! do. It is not a content browser; the content lives behind `/api/v1/*` and
//! every client can already read it.
//!
//! Auth is a token login that mints an HttpOnly session cookie. The cookie
//! never contains the token itself, so a leaked cookie cannot be replayed as an
//! API credential, and sessions die with the process.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use fqapi_core::device::MIN_POOL_SIZE;
use fqapi_core::endpoints::Params;
use serde_json::json;

use crate::http::{json_error, AppState};

const PAGE: &str = include_str!("admin_page.html");
const COOKIE: &str = "fq_admin";
const SESSION_TTL: Duration = Duration::from_secs(12 * 60 * 60);
/// Login bodies are one form field; anything larger is not a login.
const MAX_LOGIN_BODY: usize = 8 * 1024;
/// Request history rows the page asks for.
const HISTORY_ROWS: usize = 100;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin", get(page))
        .route("/admin/", get(page))
        .route("/admin/login", post(login))
        .route("/admin/logout", post(logout))
        .route("/admin/api/state", get(state))
        .route("/admin/api/tokens", post(add_token))
        .route("/admin/api/tokens/remove", post(remove_token))
        .route("/admin/api/devices/refill", post(refill))
}

async fn page() -> Response {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], PAGE).into_response()
}

async fn login(State(state): State<AppState>, request: Request) -> Response {
    let body = match axum::body::to_bytes(request.into_body(), MAX_LOGIN_BODY).await {
        Ok(body) => body,
        Err(_) => return json_error(StatusCode::BAD_REQUEST, "登录请求过大"),
    };
    let params = Params::parse(&String::from_utf8_lossy(&body));
    let token = params.get("token").unwrap_or("").trim();
    if !state.tokens.contains(token) {
        return json_error(StatusCode::UNAUTHORIZED, "令牌无效");
    }
    let session = new_session(&state);
    let cookie = format!(
        "{COOKIE}={session}; Path=/admin; HttpOnly; SameSite=Strict; Max-Age={}",
        SESSION_TTL.as_secs()
    );
    let mut response = StatusCode::NO_CONTENT.into_response();
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
}

async fn logout(State(state): State<AppState>, request: Request) -> Response {
    if let Some(session) = session_cookie(&request) {
        if let Ok(mut sessions) = state.sessions.lock() {
            sessions.remove(&session);
        }
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_static("fq_admin=; Path=/admin; HttpOnly; SameSite=Strict; Max-Age=0"),
    );
    response
}

async fn state(State(state): State<AppState>, request: Request) -> Response {
    if !session_valid(&state, &request) {
        return json_error(StatusCode::UNAUTHORIZED, "需要先登录");
    }

    let pool = &state.server.ctx.pool;
    let devices = pool.devices().await;
    // `secret_key` decrypts upstream content, so the page never sees it - only
    // whether the entry is usable.
    let devices: Vec<_> = devices
        .into_iter()
        .map(|device| {
            json!({
                "device_id": device.device_id,
                "install_id": device.install_id,
                "platform": device.platform,
                "status": device.status,
                "created_time": device.created_time,
                "last_used": device.last_used,
                "use_count": device.use_count,
                "has_key": !device.secret_key.is_empty(),
            })
        })
        .collect();

    let tokens: Vec<_> = state
        .tokens
        .list()
        .into_iter()
        .map(|token| json!({ "value": token.value, "bootstrap": token.bootstrap }))
        .collect();

    let (cache_files, cache_bytes) = cache_stats(&state.server.ctx.src_dir);

    axum::Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "devices": devices,
        "min_pool_size": MIN_POOL_SIZE,
        "ready": !devices.is_empty(),
        "tokens": tokens,
        "cache": { "files": cache_files, "bytes": cache_bytes },
        "history": state.history.as_json(HISTORY_ROWS),
    }))
    .into_response()
}

async fn add_token(State(state): State<AppState>, request: Request) -> Response {
    if !session_valid(&state, &request) {
        return json_error(StatusCode::UNAUTHORIZED, "需要先登录");
    }
    match body_token(request).await {
        Ok(token) => match state.tokens.add(&token) {
            Ok(()) => axum::Json(json!({ "ok": true })).into_response(),
            Err(message) => json_error(StatusCode::BAD_REQUEST, &message),
        },
        Err((status, message)) => json_error(status, message),
    }
}

async fn remove_token(State(state): State<AppState>, request: Request) -> Response {
    if !session_valid(&state, &request) {
        return json_error(StatusCode::UNAUTHORIZED, "需要先登录");
    }
    match body_token(request).await {
        Ok(token) => match state.tokens.remove(&token) {
            Ok(()) => axum::Json(json!({ "ok": true })).into_response(),
            Err(message) => json_error(StatusCode::BAD_REQUEST, &message),
        },
        Err((status, message)) => json_error(status, message),
    }
}

/// Tops the pool up to its minimum. `refill(n)` appends n devices, so the
/// deficit is computed here rather than passed as the minimum.
async fn refill(State(state): State<AppState>, request: Request) -> Response {
    if !session_valid(&state, &request) {
        return json_error(StatusCode::UNAUTHORIZED, "需要先登录");
    }
    let pool = &state.server.ctx.pool;
    let missing = MIN_POOL_SIZE.saturating_sub(pool.count().await);
    if missing == 0 {
        return axum::Json(json!({ "ok": true, "added": 0 })).into_response();
    }
    match pool.refill(missing).await {
        Ok(()) => axum::Json(json!({ "ok": true, "added": missing })).into_response(),
        Err(message) => json_error(StatusCode::BAD_GATEWAY, &message),
    }
}

/// The `token` field of a JSON body.
///
/// The error is `(StatusCode, &'static str)` rather than a `Response`:
/// `Response` is over 128 bytes and clippy rejects it as an error variant.
async fn body_token(request: Request) -> Result<String, (StatusCode, &'static str)> {
    let body = axum::body::to_bytes(request.into_body(), MAX_LOGIN_BODY)
        .await
        .map_err(|_| (StatusCode::BAD_REQUEST, "请求体过大"))?;
    let value: serde_json::Value =
        serde_json::from_slice(&body).map_err(|_| (StatusCode::BAD_REQUEST, "请求体不是 JSON"))?;
    let token = value
        .get("token")
        .and_then(|token| token.as_str())
        .unwrap_or("")
        .to_string();
    if token.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "缺少 token"));
    }
    Ok(token)
}

fn new_session(state: &AppState) -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let session = hex::encode(bytes);
    if let Ok(mut sessions) = state.sessions.lock() {
        // Expired entries would otherwise accumulate for the process lifetime.
        let now = Instant::now();
        sessions.retain(|_, expires| *expires > now);
        sessions.insert(session.clone(), now + SESSION_TTL);
    }
    session
}

fn session_valid(state: &AppState, request: &Request) -> bool {
    let Some(session) = session_cookie(request) else {
        return false;
    };
    let Ok(mut sessions) = state.sessions.lock() else {
        return false;
    };
    match sessions.get(&session) {
        Some(expires) if *expires > Instant::now() => true,
        Some(_) => {
            sessions.remove(&session);
            false
        }
        None => false,
    }
}

fn session_cookie(request: &Request) -> Option<String> {
    request
        .headers()
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|part| part.trim().split_once('='))
        .find(|(name, _)| *name == COOKIE)
        .map(|(_, value)| value.to_string())
}

/// Files and bytes in the media cache. The cache is flat, so one `read_dir`
/// is enough - no recursion, and nothing that walks a large tree on a page
/// refresh.
fn cache_stats(dir: &str) -> (u64, u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, 0);
    };
    let mut files = 0u64;
    let mut bytes = 0u64;
    for entry in entries.flatten() {
        if let Ok(meta) = entry.metadata() {
            if meta.is_file() {
                files += 1;
                bytes += meta.len();
            }
        }
    }
    (files, bytes)
}

/// Session map shared by the page and its API.
pub type Sessions = Arc<Mutex<HashMap<String, Instant>>>;

pub fn sessions() -> Sessions {
    Arc::new(Mutex::new(HashMap::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_stats_counts_flat_files_only() {
        let dir = std::env::temp_dir().join(format!(
            "fqapi-cache-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("nested")).expect("dir");
        std::fs::write(dir.join("a.mp4"), b"12345").expect("a");
        std::fs::write(dir.join("b.jpg"), b"123").expect("b");
        std::fs::write(dir.join("nested/ignored.mp4"), b"1234567890").expect("nested");

        let (files, bytes) = cache_stats(&dir.to_string_lossy());
        assert_eq!(files, 2, "directories are not cache files");
        assert_eq!(bytes, 8);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_cache_directory_reports_zero() {
        assert_eq!(cache_stats("/definitely/not/here"), (0, 0));
    }
}
