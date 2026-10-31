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

    /// Writes a file into the core's media cache and returns its `/src/` path.
    fn media(&self, name: &str, contents: &[u8]) -> String {
        let src = self.dir.join("src");
        std::fs::create_dir_all(&src).expect("src dir");
        std::fs::write(src.join(name), contents).expect("write media");
        format!("/src/{name}")
    }
}

fn header<'a>(response: &'a reqwest::Response, name: &str) -> &'a str {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
}

/// `Set-Cookie` carries attributes after the first `;`; only the pair matters
/// when replaying it.
fn cookie_from(response: &reqwest::Response) -> String {
    header(response, "set-cookie")
        .split(';')
        .next()
        .unwrap_or("")
        .to_string()
}

async fn login(h: &Harness, token: &str) -> String {
    let response = client()
        .post(h.url("/admin/login"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(format!("token={token}"))
        .send()
        .await
        .expect("login");
    assert_eq!(response.status(), 204, "login should succeed");
    cookie_from(&response)
}

fn json_body_request(
    client: &reqwest::Client,
    url: String,
    cookie: &str,
    body: &str,
) -> reqwest::RequestBuilder {
    client
        .post(url)
        .header("cookie", cookie)
        .header("content-type", "application/json")
        .body(body.to_string())
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
    let filters =
        FilterManager::load_with_base(dir.join("filter.json").to_str().expect("utf-8 path"), "")
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

#[tokio::test]
async fn cached_media_is_served_with_a_bearer_token() {
    let h = start("secret", false, 5).await;
    let path = h.media("clip.mp4", b"0123456789");
    let response = client()
        .get(h.url(&path))
        .bearer_auth("secret")
        .send()
        .await
        .expect("get");
    assert_eq!(response.status(), 200);
    assert_eq!(
        header(&response, "referrer-policy"),
        "no-referrer",
        "a media URL may carry the token in its query string"
    );
    assert_eq!(header(&response, "cache-control"), "private, max-age=3600");
    assert_eq!(
        response.bytes().await.expect("body").as_ref(),
        b"0123456789"
    );
}

#[tokio::test]
async fn cached_media_accepts_the_token_in_the_query() {
    let h = start("secret", false, 5).await;
    let path = h.media("img_abc.jpg", b"jpegbytes");
    let response = client()
        .get(format!("{}?token=secret", h.url(&path)))
        .send()
        .await
        .expect("get");
    assert_eq!(response.status(), 200);
    assert_eq!(response.bytes().await.expect("body").as_ref(), b"jpegbytes");
}

#[tokio::test]
async fn cached_media_without_a_valid_credential_is_rejected() {
    let h = start("secret", false, 5).await;
    let path = h.media("clip.mp4", b"x");

    let anonymous = client().get(h.url(&path)).send().await.expect("get");
    assert_eq!(anonymous.status(), 401);

    let wrong = client()
        .get(format!("{}?token=guess", h.url(&path)))
        .send()
        .await
        .expect("get");
    assert_eq!(wrong.status(), 401);
}

#[tokio::test]
async fn anonymous_read_does_not_open_cached_media() {
    let h = start("secret", true, 5).await;
    let path = h.media("clip.mp4", b"x");
    let response = client().get(h.url(&path)).send().await.expect("get");
    assert_eq!(
        response.status(),
        401,
        "cached media is the content itself, not metadata"
    );
}

#[tokio::test]
async fn the_query_token_does_not_open_api_routes() {
    let h = start("secret", false, 5).await;
    let response = client()
        .get(h.url("/api/v1/search?query=x&token=secret"))
        .send()
        .await
        .expect("get");
    assert_eq!(response.status(), 401, "the query form is media-only");
}

#[tokio::test]
async fn a_range_request_returns_206_with_content_range() {
    let h = start("secret", false, 5).await;
    let path = h.media("clip.mp4", b"0123456789");
    let response = client()
        .get(h.url(&path))
        .bearer_auth("secret")
        .header("range", "bytes=0-3")
        .send()
        .await
        .expect("get");
    assert_eq!(response.status(), 206);
    assert_eq!(header(&response, "content-range"), "bytes 0-3/10");
    assert_eq!(header(&response, "accept-ranges"), "bytes");
    assert_eq!(response.bytes().await.expect("body").as_ref(), b"0123");
}

#[tokio::test]
async fn an_unsatisfiable_range_returns_416() {
    let h = start("secret", false, 5).await;
    let path = h.media("clip.mp4", b"0123456789");
    let response = client()
        .get(h.url(&path))
        .bearer_auth("secret")
        .header("range", "bytes=100-200")
        .send()
        .await
        .expect("get");
    assert_eq!(response.status(), 416);
    assert_eq!(header(&response, "content-range"), "bytes */10");
}

/// The CENC fixture shared with the core tests: encrypted bytes, the content
/// key, the independently produced plaintext samples and the mdat offset.
fn cenc_fixture() -> (Vec<u8>, Vec<u8>, Vec<u8>, usize) {
    use base64::Engine;
    let raw = include_str!("../../fqapi-core/testdata/cenc_sample_fixture.json");
    let f: Value = serde_json::from_str(raw).expect("fixture json");
    let c = &f["cenc"];
    let decode = |v: &Value| {
        base64::engine::general_purpose::STANDARD
            .decode(v.as_str().expect("b64 field"))
            .expect("decode fixture")
    };
    let encrypted = decode(&c["full_sample_b64"]);
    let key = hex::decode(c["key_hex"].as_str().expect("key hex")).expect("key");
    let mut plain = Vec::new();
    for sample in c["full_sample_plain_b64"].as_array().expect("samples") {
        plain.extend(decode(sample));
    }
    let mdat = c["full_mdat_offset"].as_u64().expect("mdat offset") as usize;
    (encrypted, key, plain, mdat)
}

#[tokio::test]
async fn encrypted_cached_video_is_served_decrypted_over_http() {
    let h = start("secret", false, 5).await;
    let (encrypted, key, plain, mdat) = cenc_fixture();
    h.media("vid.mp4", &encrypted);
    h.media("vid.mp4.key", hex::encode(&key).as_bytes());

    let range = format!("bytes={}-{}", mdat, encrypted.len() - 1);
    let response = client()
        .get(h.url("/src/vid.mp4"))
        .bearer_auth("secret")
        .header("range", &range)
        .send()
        .await
        .expect("get");
    assert_eq!(response.status(), 206);
    assert_eq!(response.bytes().await.expect("body").as_ref(), plain);
}

#[tokio::test]
async fn encrypted_cached_video_without_a_key_is_not_served_as_ciphertext() {
    let h = start("secret", false, 5).await;
    let (encrypted, _, _, _) = cenc_fixture();
    h.media("vid.mp4", &encrypted);

    let response = client()
        .get(h.url("/src/vid.mp4"))
        .bearer_auth("secret")
        .send()
        .await
        .expect("get");
    assert_eq!(response.status(), 503);
}

#[tokio::test]
async fn the_admin_page_is_served_and_its_api_needs_a_login() {
    let h = start("secret", false, 5).await;

    let page = client().get(h.url("/admin")).send().await.expect("get");
    assert_eq!(page.status(), 200);
    let html = page.text().await.expect("body");
    assert!(
        html.contains("fqapi-server") && html.contains("/admin/api/state"),
        "the page must be the admin UI"
    );

    let anonymous = client()
        .get(h.url("/admin/api/state"))
        .send()
        .await
        .expect("get");
    assert_eq!(anonymous.status(), 401);

    let wrong = client()
        .post(h.url("/admin/login"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body("token=guess")
        .send()
        .await
        .expect("login");
    assert_eq!(wrong.status(), 401, "a wrong token must not log in");
}

#[tokio::test]
async fn the_admin_state_lists_devices_without_their_secret_keys() {
    let h = start("secret", false, 5).await;
    let cookie = login(&h, "secret").await;

    let response = client()
        .get(h.url("/admin/api/state"))
        .header("cookie", &cookie)
        .send()
        .await
        .expect("state");
    assert_eq!(response.status(), 200);
    let state = json_body(response).await;

    assert_eq!(state["ready"], true);
    let devices = state["devices"].as_array().expect("devices");
    assert_eq!(devices.len(), 5);
    assert_eq!(devices[0]["has_key"], true);
    assert!(
        devices[0].get("secret_key").is_none(),
        "secret_key must never reach the page: {devices:?}"
    );
    assert_eq!(state["tokens"][0]["bootstrap"], true);
}

#[tokio::test]
async fn a_token_added_from_the_page_works_and_can_be_revoked() {
    let h = start("secret", false, 5).await;
    let cookie = login(&h, "secret").await;

    let added = json_body_request(
        &client(),
        h.url("/admin/api/tokens"),
        &cookie,
        r#"{"token":"runtime-token-1"}"#,
    )
    .send()
    .await
    .expect("add");
    assert_eq!(added.status(), 200);

    // The new token authenticates like any other.
    let api = client()
        .get(h.url("/api/v1/search?query=x"))
        .bearer_auth("runtime-token-1")
        .send()
        .await
        .expect("search");
    assert_eq!(api.status(), 200);

    // Bootstrap tokens are the way back in, so they cannot be removed here.
    let refused = json_body_request(
        &client(),
        h.url("/admin/api/tokens/remove"),
        &cookie,
        r#"{"token":"secret"}"#,
    )
    .send()
    .await
    .expect("remove bootstrap");
    assert_eq!(refused.status(), 400);

    let removed = json_body_request(
        &client(),
        h.url("/admin/api/tokens/remove"),
        &cookie,
        r#"{"token":"runtime-token-1"}"#,
    )
    .send()
    .await
    .expect("remove runtime");
    assert_eq!(removed.status(), 200);

    let rejected = client()
        .get(h.url("/api/v1/search?query=x"))
        .bearer_auth("runtime-token-1")
        .send()
        .await
        .expect("search");
    assert_eq!(rejected.status(), 401, "a revoked token must stop working");
}

#[tokio::test]
async fn logout_invalidates_the_session() {
    let h = start("secret", false, 5).await;
    let cookie = login(&h, "secret").await;

    let before = client()
        .get(h.url("/admin/api/state"))
        .header("cookie", &cookie)
        .send()
        .await
        .expect("state");
    assert_eq!(before.status(), 200);

    client()
        .post(h.url("/admin/logout"))
        .header("cookie", &cookie)
        .send()
        .await
        .expect("logout");

    let after = client()
        .get(h.url("/admin/api/state"))
        .header("cookie", &cookie)
        .send()
        .await
        .expect("state");
    assert_eq!(after.status(), 401, "the session must be gone");
}

#[tokio::test]
async fn the_history_records_api_traffic_but_not_the_admin_page() {
    let h = start("secret", false, 5).await;
    let cookie = login(&h, "secret").await;

    client()
        .get(h.url("/api/v1/search?query=x"))
        .bearer_auth("secret")
        .send()
        .await
        .expect("search");

    let response = client()
        .get(h.url("/admin/api/state"))
        .header("cookie", &cookie)
        .send()
        .await
        .expect("state");
    let state = json_body(response).await;
    let history = state["history"].as_array().expect("history");

    assert!(
        history.iter().any(|row| row["path"] == "/api/v1/search"),
        "the API call must be recorded: {history:?}"
    );
    assert!(
        !history
            .iter()
            .any(|row| row["path"].as_str().unwrap_or("").starts_with("/admin")),
        "the page's own polling must not fill the history: {history:?}"
    );
    assert_eq!(history[0]["status"], 200);
}
