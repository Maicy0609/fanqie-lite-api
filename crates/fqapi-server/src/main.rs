//! Binary entry point: parse configuration, prepare the data directory, start
//! the core, then serve until SIGINT/SIGTERM.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use fqapi_core::runtime;
use fqapi_server::config::{self, Config, ParseOutcome};
use fqapi_server::http::{router, AppState};

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

    // Port 0 keeps the core's loopback adapter on an ephemeral port: the
    // public listener below is the only surface this process should expose.
    // See docs/UPSTREAM.md - removing that adapter is a Phase 2 item.
    runtime::init(
        paths.config.to_string_lossy().into_owned(),
        paths.pool.to_string_lossy().into_owned(),
        paths.filter.to_string_lossy().into_owned(),
        cfg.data_dir.to_string_lossy().into_owned(),
        0,
        false,
        cfg.upstream_origin.clone(),
    )
    .await?;

    let server = runtime::server().await.ok_or("the core did not start")?;
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

    // Waits for an in-flight device-pool commit before returning, so a restart
    // cannot race a rename that is already queued.
    runtime::shutdown().await?;
    eprintln!("fqapi-server stopped");
    Ok(())
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
        let dir = std::env::temp_dir().join(format!(
            "fqapi-server-main-{tag}-{}",
            std::process::id()
        ));
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
