//! Server configuration: command-line flags with environment fallbacks.
//!
//! Deliberately hand-rolled rather than pulling in an argument parser: the flag
//! set is small and fixed, and this crate's dependency budget is spent on the
//! HTTP layer instead.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

pub const USAGE: &str = "\
fqapi-server - HTTP front end for fqapi_core

Usage: fqapi-server [options]

Options:
  --data-dir <path>        Runtime data directory (default ./data)
  --listen <addr:port>     Listen address (default 127.0.0.1:18921)
  --token <token>          API token; repeatable or comma-separated
  --anonymous-read         Let read routes through without a token
  --max-concurrency <n>    In-flight request cap (default 60)
  --timeout <secs>         Per-request deadline in seconds (default 60)
  --cors-origin <origin>   Allowed CORS origin; repeatable or comma-separated
  --upstream-origin <url>  Redirect every upstream call (offline testing)
  -h, --help               Show this help

Environment (used when the matching flag is absent):
  FQ_DATA_DIR  FQ_LISTEN  FQ_API_TOKENS  FQ_ANONYMOUS_READ
  FQ_MAX_CONCURRENCY  FQ_TIMEOUT_SECS  FQ_CORS_ORIGINS  FQ_UPSTREAM_ORIGIN
";

#[derive(Debug, Clone)]
pub struct Config {
    pub data_dir: PathBuf,
    pub listen: SocketAddr,
    /// Accepted bearer tokens. Empty means "refuse to start": an open listener
    /// would let anyone spend this host's device identities.
    pub tokens: Vec<String>,
    pub anonymous_read: bool,
    pub max_concurrency: usize,
    pub timeout: Duration,
    /// Empty means "no CORS layer"; native clients do not need one.
    pub cors_origins: Vec<String>,
    /// Offline test seam, passed straight to the core. Empty in normal use.
    pub upstream_origin: String,
}

pub enum ParseOutcome {
    Run(Box<Config>),
    Help,
}

/// Parses the process arguments and environment.
pub fn parse() -> Result<ParseOutcome, String> {
    parse_from(std::env::args().skip(1), |key| std::env::var(key).ok())
}

/// Testable core of [`parse`]: arguments and environment are injected.
pub fn parse_from(
    args: impl IntoIterator<Item = String>,
    env: impl Fn(&str) -> Option<String>,
) -> Result<ParseOutcome, String> {
    let mut data_dir: Option<String> = None;
    let mut listen: Option<String> = None;
    let mut tokens: Vec<String> = Vec::new();
    let mut anonymous_read = false;
    let mut max_concurrency: Option<usize> = None;
    let mut timeout_secs: Option<u64> = None;
    let mut cors_origins: Vec<String> = Vec::new();
    let mut upstream_origin: Option<String> = None;

    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| -> Result<String, String> {
            args.next()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match arg.as_str() {
            "-h" | "--help" => return Ok(ParseOutcome::Help),
            "--data-dir" => data_dir = Some(value("--data-dir")?),
            "--listen" => listen = Some(value("--listen")?),
            "--token" => push_list(&mut tokens, &value("--token")?),
            "--anonymous-read" => anonymous_read = true,
            "--max-concurrency" => {
                let raw = value("--max-concurrency")?;
                max_concurrency = Some(
                    raw.parse()
                        .map_err(|_| format!("--max-concurrency is not a number: {raw}"))?,
                );
            }
            "--timeout" => {
                let raw = value("--timeout")?;
                timeout_secs = Some(
                    raw.parse()
                        .map_err(|_| format!("--timeout is not a number: {raw}"))?,
                );
            }
            "--cors-origin" => push_list(&mut cors_origins, &value("--cors-origin")?),
            "--upstream-origin" => upstream_origin = Some(value("--upstream-origin")?),
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    let data_dir = data_dir
        .or_else(|| env("FQ_DATA_DIR"))
        .unwrap_or_else(|| "data".to_string());
    let listen = listen
        .or_else(|| env("FQ_LISTEN"))
        .unwrap_or_else(|| "127.0.0.1:18921".to_string());
    let listen: SocketAddr = listen
        .parse()
        .map_err(|_| format!("--listen is not an address:port: {listen}"))?;

    if let Some(raw) = env("FQ_API_TOKENS") {
        push_list(&mut tokens, &raw);
    }
    if !anonymous_read {
        anonymous_read = matches!(env("FQ_ANONYMOUS_READ").as_deref(), Some("1" | "true"));
    }
    if let Some(raw) = env("FQ_CORS_ORIGINS") {
        push_list(&mut cors_origins, &raw);
    }

    let max_concurrency = match max_concurrency {
        Some(n) => n,
        None => match env("FQ_MAX_CONCURRENCY") {
            Some(raw) => raw
                .parse()
                .map_err(|_| format!("FQ_MAX_CONCURRENCY is not a number: {raw}"))?,
            None => 60,
        },
    };
    if max_concurrency == 0 {
        return Err("--max-concurrency must be at least 1".to_string());
    }

    let timeout_secs = match timeout_secs {
        Some(n) => n,
        None => match env("FQ_TIMEOUT_SECS") {
            Some(raw) => raw
                .parse()
                .map_err(|_| format!("FQ_TIMEOUT_SECS is not a number: {raw}"))?,
            None => 60,
        },
    };
    if timeout_secs == 0 {
        return Err("--timeout must be at least 1 second".to_string());
    }

    Ok(ParseOutcome::Run(Box::new(Config {
        data_dir: PathBuf::from(data_dir),
        listen,
        tokens,
        anonymous_read,
        max_concurrency,
        timeout: Duration::from_secs(timeout_secs),
        cors_origins,
        upstream_origin: upstream_origin
            .or_else(|| env("FQ_UPSTREAM_ORIGIN"))
            .unwrap_or_default(),
    })))
}

/// Accepts both `--token a --token b` and `--token a,b`.
fn push_list(target: &mut Vec<String>, raw: &str) {
    for part in raw.split(',') {
        let part = part.trim();
        if !part.is_empty() && !target.iter().any(|existing| existing == part) {
            target.push(part.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str], env: &[(&str, &str)]) -> Result<Config, String> {
        let env: Vec<(String, String)> = env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        match parse_from(args.iter().map(|s| s.to_string()), |key| {
            env.iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
        })? {
            ParseOutcome::Run(cfg) => Ok(*cfg),
            ParseOutcome::Help => Err("help".to_string()),
        }
    }

    #[test]
    fn defaults_are_loopback_and_conservative() {
        let cfg = run(&[], &[]).expect("defaults");
        assert_eq!(cfg.listen.to_string(), "127.0.0.1:18921");
        assert_eq!(cfg.data_dir.to_str(), Some("data"));
        assert_eq!(cfg.max_concurrency, 60);
        assert_eq!(cfg.timeout, Duration::from_secs(60));
        assert!(cfg.tokens.is_empty(), "no token is configured by default");
        assert!(!cfg.anonymous_read);
        assert!(cfg.cors_origins.is_empty());
    }

    #[test]
    fn tokens_accept_repeats_and_commas_without_duplicates() {
        let cfg = run(&["--token", "a,b", "--token", "b", "--token", "c"], &[]).expect("tokens");
        assert_eq!(cfg.tokens, vec!["a", "b", "c"]);
    }

    #[test]
    fn flags_win_over_environment() {
        let cfg = run(
            &["--listen", "0.0.0.0:9000", "--timeout", "5"],
            &[("FQ_LISTEN", "127.0.0.1:1"), ("FQ_TIMEOUT_SECS", "99")],
        )
        .expect("flags");
        assert_eq!(cfg.listen.to_string(), "0.0.0.0:9000");
        assert_eq!(cfg.timeout, Duration::from_secs(5));
    }

    #[test]
    fn environment_fills_the_gaps() {
        let cfg = run(
            &[],
            &[
                ("FQ_API_TOKENS", "x,y"),
                ("FQ_ANONYMOUS_READ", "1"),
                ("FQ_CORS_ORIGINS", "https://a.example"),
                ("FQ_UPSTREAM_ORIGIN", "http://127.0.0.1:1"),
            ],
        )
        .expect("env");
        assert_eq!(cfg.tokens, vec!["x", "y"]);
        assert!(cfg.anonymous_read);
        assert_eq!(cfg.cors_origins, vec!["https://a.example"]);
        assert_eq!(cfg.upstream_origin, "http://127.0.0.1:1");
    }

    #[test]
    fn a_flag_missing_its_value_is_an_error() {
        assert!(run(&["--token"], &[]).is_err());
        assert!(run(&["--listen"], &[]).is_err());
    }

    #[test]
    fn unknown_arguments_and_zero_limits_are_rejected() {
        assert!(run(&["--nope"], &[]).is_err());
        assert!(run(&["--max-concurrency", "0"], &[]).is_err());
        assert!(run(&["--timeout", "0"], &[]).is_err());
        assert!(run(&["--listen", "not-an-address"], &[]).is_err());
    }
}
