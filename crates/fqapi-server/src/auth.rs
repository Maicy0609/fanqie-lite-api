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
//! Two route families keep requiring a token even under `--anonymous-read`:
//!
//! * `/api/v1/account/*` forwards a user's upstream cookie, which must never be
//!   reachable by an unauthenticated caller;
//! * the device-bound write routes (`fqapi_core::dispatch::requires_capability`)
//!   post comments and danmaku as this installation's device identity.

use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use fqapi_core::dispatch::requires_capability;

use crate::http::{json_error, AppState};

pub async fn require_token(State(state): State<AppState>, request: Request, next: Next) -> Response {
    if !needs_token(request.uri().path(), state.anonymous_read) {
        return next.run(request).await;
    }

    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(bearer);
    if let Some(presented) = presented {
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
    if requires_capability(path) {
        return true;
    }
    !anonymous_read
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
