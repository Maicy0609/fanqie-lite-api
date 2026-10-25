//! Probes.
//!
//! Both routes are answered without a token: a container or load balancer has
//! no way to present one, and neither reveals content. They reuse the core's
//! own `/health` route rather than reading pool state directly, so there is one
//! definition of "how many devices are usable".

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use fqapi_core::dispatch::{self, Request};

use crate::http::{json_error, AppState};

const JSON_CT: &str = "application/json; charset=utf-8";

/// Liveness: the process serves requests. Always 200 while the dispatcher
/// answers, even with an empty device pool - that is what `/readyz` is for.
pub async fn healthz(State(state): State<AppState>) -> Response {
    match device_report(&state).await {
        Some((_, body)) => {
            (StatusCode::OK, [(header::CONTENT_TYPE, JSON_CT)], body).into_response()
        }
        None => json_error(StatusCode::INTERNAL_SERVER_ERROR, "health check failed"),
    }
}

/// Readiness: at least one usable device. A cold start registers the whole pool
/// (five devices, each with a deliberate two-second pause), so this stays 503
/// for the first ten to twenty seconds.
pub async fn readyz(State(state): State<AppState>) -> Response {
    match device_report(&state).await {
        Some((devices, body)) if devices > 0 => {
            (StatusCode::OK, [(header::CONTENT_TYPE, JSON_CT)], body).into_response()
        }
        Some((devices, _)) => (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CONTENT_TYPE, JSON_CT)],
            serde_json::json!({ "status": "starting", "devices": devices }).to_string(),
        )
            .into_response(),
        None => json_error(StatusCode::INTERNAL_SERVER_ERROR, "health check failed"),
    }
}

/// The device count and the core's `/health` payload, or `None` when the
/// dispatcher could not produce one.
///
/// `Option` rather than `Result<_, Response>`: `Response` is a large type, and
/// returning it as an error variant makes every `Result` in this file carry it.
async fn device_report(state: &AppState) -> Option<(u64, Vec<u8>)> {
    let request = Request {
        method: "GET".to_string(),
        path: "/health".to_string(),
        ..Default::default()
    };
    let body = dispatch::dispatch(&state.server, &request)
        .await
        .into_bytes()
        .await
        .ok()?;
    let devices = count_devices(&body);
    Some((devices, body))
}

fn count_devices(body: &[u8]) -> u64 {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("devices").and_then(|d| d.as_u64()))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::count_devices;

    #[test]
    fn reads_the_device_count_from_the_core_health_payload() {
        assert_eq!(count_devices(br#"{"status":"ok","devices":5}"#), 5);
        assert_eq!(count_devices(br#"{"status":"ok","devices":0}"#), 0);
    }

    #[test]
    fn an_unexpected_payload_reads_as_zero_rather_than_panicking() {
        assert_eq!(count_devices(b"not json"), 0);
        assert_eq!(count_devices(br#"{"status":"ok"}"#), 0);
        assert_eq!(count_devices(br#"{"devices":"five"}"#), 0);
    }
}
