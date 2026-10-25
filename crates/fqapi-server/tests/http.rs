//! Phase 1 smoke tests: the HTTP layer against a scripted upstream.
//!
//! The core is built directly (`Ctx` + `Server`) instead of through
//! `runtime::init`, so each test owns its own dispatcher, a seeded device pool
//! and a local mock upstream. Nothing here touches the network, a real device,
//! or process-global state - which is also why these tests can run in parallel.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::http::header;
use axum::response::{IntoResponse, Response as AxumResponse};
use axum::Router;
use fqapi_core::config::Config as CoreConfig;
use fqapi_core::device::DevicePool;
use fqapi_core::endpoints::{Ctx, Server};
use fqapi_core::filter::FilterManager;
use fqapi_core::sign::Manager;
use fqapi_core::upstream::UpstreamClient;
use fqapi_server::config::Config;
use fqapi_server::http::{router, AppState};
use serde_json::{json, Value};

/// `reqwest` is built with `rustls-no-provider`, so the process-wide crypto
/// provider has to be installed before the first client is constructed. The
/// core does this lazily inside `HttpTransport`, but the test client is built
/// independently and must not depend on that ordering.
fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

struct Harness {
    dir: PathBuf,
    base: String,
    app: tokio::task::JoinHandle<()>,
    upstream: tokio::task::JoinHandle<()>,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.app.abort();
        self.upstream.abort();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Harness {
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }
}

/// Test directories must be unique per *call*, not per timestamp: eight tests
/// start at once, and `SystemTime::now()` on Windows does not have the
/// resolution to tell them apart. Colliding directories made one test's
/// cleanup delete another's files mid-run.
static DIR_SEQ: AtomicU64 = AtomicU64::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let n = DIR_SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "fqapi-server-http-{tag}-{}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// Writes a pool of `count` devices so `pool.get()` never tries to register.
fn seed_pool(dir: &Path, count: usize) {
    let devices: Vec<Value> = (0..count)
        .map(|i| {
            json!({
                "device_id": format!("{i:016}"),
                "install_id": format!("{}", 9_000_000_000_000_000_000u64 + i as u64),
                "secret_key": format!("{i:032x}"),
                "platform": "android",
                "status": "active",
                "created_time": "2026-01-01 00:00:00",
                "last_used": format!("2026-01-01 00:00:0{i}"),
                "use_count": i,
                "cdid": format!("cdid-{i}"),
                "openudid": format!("{i:016}"),
            })
        })
        .collect();
    let pool = json!({ "android": devices, "last_update": "2026-01-01 00:00:00" });
    std::fs::write(
        dir.join("device_pool.json"),
        serde_json::to_vec(&pool).expect("pool json"),
    )
    .expect("write pool");
}

async fn build_core(dir: &Path, upstream_origin: &str) -> Arc<Server> {
    let config_path = dir.join("config.json");
    std::fs::write(&config_path, r#"{"algorithm_type":"8404","port":0}"#).expect("config");
    let core_config = CoreConfig::load(config_path.to_str().expect("utf-8 path")).expect("load");
    let pool = DevicePool::with_mock_origin(dir.join("device_pool.json"), upstream_origin)
        .await
        .expect("pool");
    let upstream = UpstreamClient::with_mock_origin(pool.clone(), Manager::new(), upstream_origin)
        .expect("upstream");
    let filters = FilterManager::load_with_base(
        dir.join("filter.json").to_str().expect("utf-8 path"),
        "",
    )
    .expect("filters");
    let ctx = Arc::new(Ctx {
        up: Arc::new(upstream),
        pool,
        cfg: Arc::new(core_config),
        src_dir: dir.join("src").to_string_lossy().into_owned(),
        filters: Arc::new(filters),
    });
    Arc::new(Server::new(ctx))
}

async fn mock_upstream() -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new().fallback(mock_handler);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock");
    let addr = listener.local_addr().expect("addr");
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), task)
}

/// Answers every upstream call with a recognisable payload; the search path gets
/// a book so the test can tell "reached the upstream" from "empty envelope".
async fn mock_handler(request: axum::extract::Request) -> AxumResponse {
    let path = request.uri().path().to_string();
    let body = if path.contains("/search/") {
        json!({ "code": 0, "data": { "books": [{ "book_id": "1", "title": "mock" }] } })
    } else {
        json!({ "code": 0, "data": {} })
    };
    (
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

async fn start(token: &str, anonymous_read: bool, devices: usize) -> Harness {
    install_crypto_provider();
    let dir = temp_dir(if anonymous_read { "anon" } else { "auth" });
    seed_pool(&dir, devices);
    let (origin, upstream) = mock_upstream().await;
    let server = build_core(&dir, &origin).await;

    let cfg = Config {
        data_dir: dir.clone(),
        listen: "127.0.0.1:0".parse().expect("addr"),
        tokens: vec![token.to_string()],
        anonymous_read,
        max_concurrency: 8,
        timeout: Duration::from_secs(10),
        cors_origins: Vec::new(),
        upstream_origin: String::new(),
    };
    let app = router(AppState::from_config(server, &cfg)).expect("router");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind app");
    let addr = listener.local_addr().expect("addr");
    let app_task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    Harness {
        dir,
        base: format!("http://{addr}"),
        app: app_task,
        upstream,
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("client")
}

async fn json_body(response: reqwest::Response) -> Value {
    let text = response.text().await.expect("body");
    serde_json::from_str(&text).expect("json body")
}

#[tokio::test]
async fn healthz_is_open_and_reports_the_pool() {
    let h = start("secret", false, 5).await;
    let response = client().get(h.url("/healthz")).send().await.expect("get");
    assert_eq!(response.status(), 200, "liveness must not need a token");
    assert_eq!(json_body(response).await["devices"], 5);
}

#[tokio::test]
async fn readyz_is_ready_when_the_pool_has_devices() {
    let h = start("secret", false, 5).await;
    let response = client().get(h.url("/readyz")).send().await.expect("get");
    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn readyz_reports_starting_on_an_empty_pool() {
    let h = start("secret", false, 0).await;
    let response = client().get(h.url("/readyz")).send().await.expect("get");
    assert_eq!(response.status(), 503);
    assert_eq!(json_body(response).await["status"], "starting");
}

#[tokio::test]
async fn api_without_a_token_is_rejected() {
    let h = start("secret", false, 5).await;
    let response = client()
        .get(h.url("/api/v1/search?query=x"))
        .send()
        .await
        .expect("get");
    assert_eq!(response.status(), 401);
    assert_eq!(json_body(response).await["success"], false);
}

#[tokio::test]
async fn a_wrong_token_is_rejected() {
    let h = start("secret", false, 5).await;
    let response = client()
        .get(h.url("/api/v1/search?query=x"))
        .bearer_auth("guess")
        .send()
        .await
        .expect("get");
    assert_eq!(response.status(), 401);
}

#[tokio::test]
async fn api_with_a_token_reaches_the_upstream() {
    let h = start("secret", false, 5).await;
    let response = client()
        .get(h.url("/api/v1/search?query=x"))
        .bearer_auth("secret")
        .send()
        .await
        .expect("get");
    assert_eq!(response.status(), 200);
    let body = json_body(response).await;
    assert_eq!(
        body["data"]["books"][0]["title"], "mock",
        "the response must be the upstream payload, unchanged: {body}"
    );
}

#[tokio::test]
async fn anonymous_read_opens_reads_but_not_device_bound_writes() {
    let h = start("secret", true, 5).await;

    let read = client()
        .get(h.url("/api/v1/search?query=x"))
        .send()
        .await
        .expect("read");
    assert_eq!(read.status(), 200, "a read route may skip the token");

    let write = client()
        .post(h.url("/api/v1/series/1/comments/add?content=hi"))
        .send()
        .await
        .expect("write");
    assert_eq!(
        write.status(),
        401,
        "a device-bound write must still require a token"
    );
}

#[tokio::test]
async fn an_unknown_path_is_a_json_404() {
    let h = start("secret", false, 5).await;
    let response = client()
        .get(h.url("/api/v1/nope"))
        .bearer_auth("secret")
        .send()
        .await
        .expect("get");
    assert_eq!(response.status(), 404);
    assert_eq!(json_body(response).await["success"], false);
}

#[tokio::test]
async fn a_bad_request_parameter_is_a_400_from_the_core() {
    let h = start("secret", false, 5).await;
    let response = client()
        .get(h.url("/api/v1/chapters/batch"))
        .bearer_auth("secret")
        .send()
        .await
        .expect("get");
    assert_eq!(
        response.status(),
        400,
        "the core's BadRequest must survive the transport"
    );
}
