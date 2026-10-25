//! Bearer-token authentication for the public listener.
//!
//! Upstream's loopback adapter guards only the three device-bound write routes
//! with a per-launch capability, because "same phone, other app" was the threat
//! model and read routes had to stay open for external readers. On a public
//! listener that model is backwards: whoever can reach the port can spend this
//! host's device identities. So the default is inverted - every route needs a
//! token, and `--anonymous-read` is the explicit opt-out for readers such as
//! Legado book sources.
//!
//! Three route families keep requiring a token even under `--anonymous-read`:
//!
//! * `/api/v1/account/*` forwards a user's upstream cookie, which must never be
//!   reachable by an unauthenticated caller;
//! * the device-bound write routes (`fqapi_core::dispatch::requires_capability`)
//!   post comments and danmaku as this installation's device identity;
//! * `/src/*` is the decrypted content itself, not metadata.
//!
//! `/src/*` additionally accepts the token as a `?token=` query parameter. An
//! `<img>` or `<video>` tag cannot send an `Authorization` header, and the
//! alternative - rewriting `/src/` paths in responses into signed URLs - would
//! break the existing client, which recognises media by that exact prefix
//! (`fqapp/lib/services/episode_source_cache.dart`). The cost is that the token
//! can appear in a browser's history or a proxy log; responses therefore carry
//! `Referrer-Policy: no-referrer` and `Cache-Control: private`, and the query
//! form is accepted for media paths only.

use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use fqapi_core::dispatch::requires_capability;
use fqapi_core::endpoints::Params;

use crate::http::{is_media_path, json_error, AppState};

pub async fn require_token(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let path = request.uri().path().to_string();
    if !needs_token(&path, state.anonymous_read) {
        return next.run(request).await;
    }

    if let Some(presented) = presented_token(&request, &path) {
        if state
            .tokens
            .iter()
            .any(|known| constant_time_eq(known.as_bytes(), presented.as_bytes()))
        {
            return next.run(request).await;
        }
    }

    json_error(StatusCode::UNAUTHORIZED, "missing or invalid API token")
}

fn needs_token(path: &str, anonymous_read: bool) -> bool {
    if path.starts_with("/api/v1/account/") {
        return true;
    }
    if is_media_path(path) {
        return true;
    }
    if requires_capability(path) {
        return true;
    }
    !anonymous_read
}

/// The credential this request presents, if any.
///
/// The query form is deliberately limited to media: accepting it anywhere would
/// turn every URL into a bearer credential.
fn presented_token(request: &Request, path: &str) -> Option<String> {
    if let Some(token) = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(bearer)
    {
        return Some(token.to_string());
    }
    if is_media_path(path) {
        let params = Params::parse(request.uri().query().unwrap_or(""));
        return params.get("token").map(str::to_string);
    }
    None
}

/// Extracts the token from `Bearer <token>`, scheme case-insensitively.
fn bearer(value: &str) -> Option<&str> {
    let (scheme, token) = value.trim().split_once(' ')?;
    if scheme.eq_ignore_ascii_case("bearer") {
        let token = token.trim();
        if !token.is_empty() {
            return Some(token);
        }
    }
    None
}

/// Byte-wise comparison with no early exit, so a caller cannot learn a token
/// prefix from response timing. The length check is not secret-dependent: token
/// length comes from configuration, not from the value.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_by_default() {
        assert!(needs_token("/api/v1/search", false));
        assert!(needs_token("/health", false));
    }

    #[test]
    fn anonymous_read_opens_plain_reads_only() {
        assert!(!needs_token("/api/v1/search", true));
        assert!(!needs_token("/api/v1/books/1/directory", true));
        // Device-bound writes stay closed.
        assert!(needs_token("/api/v1/series/1/comments/add", true));
        assert!(needs_token("/api/v1/comments/1/digg", true));
        // Account routes carry a user cookie.
        assert!(needs_token("/api/v1/account/fanqie/library", true));
        assert!(needs_token("/api/v1/account/fanqie/history", true));
    }

    #[test]
    fn cached_media_is_never_anonymous() {
        // `/src/*` is the decrypted content, not metadata: opening it with
        // `--anonymous-read` would publish every cached episode.
        assert!(needs_token("/src", true));
        assert!(needs_token("/src/clip.mp4", true));
        assert!(needs_token("/src/img_abc.jpg", true));
    }

    #[test]
    fn the_media_namespace_is_matched_exactly() {
        assert!(is_media_path("/src"));
        assert!(is_media_path("/src/"));
        assert!(is_media_path("/src/a.mp4"));
        // A prefix match without the separator would capture unrelated paths.
        assert!(!is_media_path("/srcx"));
        assert!(!is_media_path("/api/v1/src/a.mp4"));
        assert!(!is_media_path("/api/v1/search"));
    }

    #[test]
    fn bearer_scheme_is_case_insensitive_and_needs_a_value() {
        assert_eq!(bearer("Bearer abc"), Some("abc"));
        assert_eq!(bearer("bearer abc"), Some("abc"));
        assert_eq!(bearer("  Bearer   abc  "), Some("abc"));
        assert_eq!(bearer("Bearer "), None);
        assert_eq!(bearer("abc"), None);
        assert_eq!(bearer("Basic abc"), None);
    }

    #[test]
    fn constant_time_equality_matches_ordinary_equality() {
        assert!(constant_time_eq(b"token", b"token"));
        assert!(!constant_time_eq(b"token", b"tokeN"));
        assert!(!constant_time_eq(b"token", b"token "));
        assert!(!constant_time_eq(b"", b"x"));
        assert!(constant_time_eq(b"", b""));
    }
}
