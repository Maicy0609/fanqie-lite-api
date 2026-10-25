//! Binary entry point: parse configuration, prepare the data directory, start
//! the core, then serve until SIGINT/SIGTERM.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use fqapi_core::device::{DevicePool, MIN_POOL_SIZE};
use fqapi_core::runtime;
use fqapi_core::server::ListenMode;
use fqapi_server::config::{self, Config, ParseOutcome};
use fqapi_server::http::{router, AppState};
use fqapi_server::poolkey;

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("fqapi-server: {message}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), String> {
    let cfg = match config::parse()? {
        ParseOutcome::Help => {
            print!("{}", config::USAGE);
            return Ok(());
        }
        ParseOutcome::Run(cfg) => *cfg,
    };

    if cfg.tokens.is_empty() {
        return Err(
            "no API token configured. Pass --token <value> or set FQ_API_TOKENS; \
             an unauthenticated listener would let anyone spend this host's \
             device identities."
                .to_string(),
        );
    }

    let paths = prepare_data_dir(&cfg)?;

    // Before `init`, so the pool reads an encrypted file on its first load and
    // writes one on its first save.
    let pool_key = poolkey::load_or_create(
        &cfg.data_dir,
        &paths.pool,
        std::env::var("FQ_POOL_KEY").ok(),
    )?;
    runtime::set_device_pool_key(pool_key.to_vec())?;
    // A pool file written before the key existed is still plaintext, and
    // `save` only migrates it on the next mutation - which an idle host may
    // never reach.
    let migrate_pool = poolkey::needs_migration(&paths.pool);

    // `Disabled`: this process is the only listener. The loopback adapter's
    // read routes answer any local client without a token, which would be a
    // side door next to the authenticated public listener.
    runtime::init_core(
        paths.config.to_string_lossy().into_owned(),
        paths.pool.to_string_lossy().into_owned(),
        paths.filter.to_string_lossy().into_owned(),
        cfg.data_dir.to_string_lossy().into_owned(),
        0,
        ListenMode::Disabled,
        if cfg.upstream_origin.trim().is_empty() {
            None
        } else {
            Some(cfg.upstream_origin.clone())
        },
    )
    .await?;

    let server = runtime::server().await.ok_or("the core did not start")?;
    if migrate_pool {
        server
            .ctx
            .pool
            .persist()
            .await
            .map_err(|e| format!("encrypt the existing device pool: {e}"))?;
    }
    // Fill the pool without waiting for the first request, so `/readyz` flips
    // on its own. Registration takes seconds per device, so this must not block
    // startup.
    let warm_up = tokio::spawn(warm_up(server.ctx.pool.clone()));
    let app = router(AppState::from_config(server, &cfg))?;

    let listener = tokio::net::TcpListener::bind(cfg.listen)
        .await
        .map_err(|e| format!("listen {}: {e}", cfg.listen))?;
    let bound = listener.local_addr().map_err(|e| e.to_string())?;

    eprintln!("fqapi-server listening on http://{bound}");
    eprintln!("  data dir:       {}", cfg.data_dir.display());
    eprintln!("  tokens:         {}", cfg.tokens.len());
    eprintln!("  anonymous read: {}", cfg.anonymous_read);
    eprintln!("  deadline:       {}s", cfg.timeout.as_secs());
    eprintln!("  concurrency:    {}", cfg.max_concurrency);

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| format!("serve: {e}"))?;

    // A warm-up still in flight would otherwise keep registering devices while
    // the pool is being retired.
    warm_up.abort();
    // Waits for an in-flight device-pool commit before returning, so a restart
    // cannot race a rename that is already queued.
    runtime::shutdown().await?;
    eprintln!("fqapi-server stopped");
    Ok(())
}

/// Tops the pool up to its minimum. The core does the same on every `get`, so
/// this only exists to make readiness independent of traffic.
///
/// `refill(n)` means "add up to n devices", not "reach n", so the deficit has
/// to be computed here: passing `MIN_POOL_SIZE` on a healthy pool would keep
/// registering devices until it hit the maximum.
async fn warm_up(pool: Arc<DevicePool>) {
    let missing = MIN_POOL_SIZE.saturating_sub(pool.count().await);
    if missing == 0 {
        return;
    }
    if let Err(error) = pool.refill(missing).await {
        log::warn!("device pool warm-up incomplete: {error}");
    }
}

struct Paths {
    config: PathBuf,
    pool: PathBuf,
    filter: PathBuf,
}

/// Creates the data directory and seeds the two files the core expects.
/// `device_pool.json` is deliberately not created: the core writes it on the
/// first save, and an empty file would be treated as corrupt.
fn prepare_data_dir(cfg: &Config) -> Result<Paths, String> {
    let config_dir = cfg.data_dir.join("config");
    std::fs::create_dir_all(&config_dir)
        .map_err(|e| format!("create {}: {e}", config_dir.display()))?;

    let config_path = config_dir.join("config.json");
    write_if_missing(&config_path, "{\"algorithm_type\":\"8404\",\"port\":0}\n")?;
    let filter_path = config_dir.join("filter.json");
    write_if_missing(&filter_path, "{\"routes\":{}}\n")?;

    Ok(Paths {
        config: config_path,
        pool: config_dir.join("device_pool.json"),
        filter: filter_path,
    })
}

fn write_if_missing(path: &Path, contents: &str) -> Result<(), String> {
    if path.exists() {
        return Ok(());
    }
    std::fs::write(path, contents).map_err(|e| format!("write {}: {e}", path.display()))
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    eprintln!("fqapi-server: shutting down");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("fqapi-server-main-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn config(data_dir: PathBuf) -> Config {
        Config {
            data_dir,
            listen: "127.0.0.1:0".parse().expect("addr"),
            tokens: vec!["t".to_string()],
            anonymous_read: false,
            max_concurrency: 4,
            timeout: std::time::Duration::from_secs(5),
            cors_origins: Vec::new(),
            upstream_origin: String::new(),
        }
    }

    #[test]
    fn seeds_the_config_files_but_not_the_pool() {
        let dir = temp_dir("seed");
        let paths = prepare_data_dir(&config(dir.clone())).expect("prepare");

        assert!(paths.config.exists(), "config.json is seeded");
        assert!(paths.filter.exists(), "filter.json is seeded");
        assert!(
            !paths.pool.exists(),
            "an empty device_pool.json would read as corrupt"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn existing_files_are_never_overwritten() {
        let dir = temp_dir("keep");
        let cfg = config(dir.clone());
        let paths = prepare_data_dir(&cfg).expect("first");
        std::fs::write(&paths.config, "{\"algorithm_type\":\"custom\"}").expect("edit");

        prepare_data_dir(&cfg).expect("second");
        let kept = std::fs::read_to_string(&paths.config).expect("read");
        assert!(
            kept.contains("custom"),
            "a second start must not clobber an edited config: {kept}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
