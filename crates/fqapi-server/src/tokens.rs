//! API tokens: the ones the process was started with, plus ones added at
//! runtime from the admin page.
//!
//! Bootstrap tokens (`--token` / `FQ_API_TOKENS`) are never persisted and never
//! removable: they are the way back in if every runtime token is revoked by
//! mistake, and they are what the operator already has in their compose file.
//! Runtime tokens live in `<data-dir>/tokens.json` so a restart does not log
//! every client out.

use std::path::{Path, PathBuf};
use std::sync::RwLock;

const FILE_NAME: &str = "tokens.json";

/// Shortest token the admin page accepts. Not a security boundary - it only
/// stops a two-character token from being added by accident.
const MIN_LEN: usize = 8;

pub struct TokenStore {
    path: PathBuf,
    bootstrap: Vec<String>,
    added: RwLock<Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct TokenView {
    pub value: String,
    /// True when the token came from the command line or environment.
    pub bootstrap: bool,
}

impl TokenStore {
    pub fn load(data_dir: &Path, bootstrap: Vec<String>) -> Self {
        let path = data_dir.join(FILE_NAME);
        let added = read_tokens(&path);
        TokenStore {
            path,
            bootstrap,
            added: RwLock::new(added),
        }
    }

    /// Whether `candidate` is a live token.
    ///
    /// Every entry is compared, and the results are OR-ed, so neither a miss
    /// nor a hit is distinguishable from its position in the list by timing.
    pub fn contains(&self, candidate: &str) -> bool {
        if candidate.is_empty() {
            return false;
        }
        let added = self.added.read().map(|t| t.clone()).unwrap_or_default();
        let mut matched = false;
        for known in self.bootstrap.iter().chain(added.iter()) {
            matched |= constant_time_eq(known.as_bytes(), candidate.as_bytes());
        }
        matched
    }

    pub fn list(&self) -> Vec<TokenView> {
        let added = self.added.read().map(|t| t.clone()).unwrap_or_default();
        self.bootstrap
            .iter()
            .map(|value| TokenView {
                value: value.clone(),
                bootstrap: true,
            })
            .chain(added.into_iter().map(|value| TokenView {
                value,
                bootstrap: false,
            }))
            .collect()
    }

    pub fn add(&self, value: &str) -> Result<(), String> {
        let value = value.trim();
        if value.len() < MIN_LEN {
            return Err(format!("令牌至少 {MIN_LEN} 个字符"));
        }
        if value.chars().any(|c| c.is_control()) {
            return Err("令牌不能包含控制字符".to_string());
        }
        if self.contains(value) {
            return Err("这个令牌已经存在".to_string());
        }
        let mut tokens = self
            .added
            .write()
            .map_err(|_| "token lock poisoned".to_string())?;
        tokens.push(value.to_string());
        let snapshot = tokens.clone();
        drop(tokens);
        self.persist(&snapshot)?;
        Ok(())
    }

    pub fn remove(&self, value: &str) -> Result<(), String> {
        let value = value.trim();
        if self.bootstrap.iter().any(|known| known == value) {
            return Err("启动参数提供的令牌不能在这里删除".to_string());
        }
        let mut tokens = self
            .added
            .write()
            .map_err(|_| "token lock poisoned".to_string())?;
        let before = tokens.len();
        tokens.retain(|known| known != value);
        if tokens.len() == before {
            return Err("没有这个令牌".to_string());
        }
        let snapshot = tokens.clone();
        drop(tokens);
        self.persist(&snapshot)?;
        Ok(())
    }

    /// Atomic write: a torn `tokens.json` would lock every client out.
    fn persist(&self, tokens: &[String]) -> Result<(), String> {
        let body = serde_json::json!({ "tokens": tokens }).to_string();
        let tmp = self.path.with_extension("json.tmp");
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&tmp, body.as_bytes()).map_err(|e| format!("write tokens: {e}"))?;
        restrict(&tmp);
        std::fs::rename(&tmp, &self.path).map_err(|e| format!("commit tokens: {e}"))
    }
}

/// Byte-wise comparison with no early exit.
///
/// The length check is not secret-dependent in practice: token length comes
/// from configuration, not from the value being guessed.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn read_tokens(path: &Path) -> Vec<String> {
    let Ok(data) = std::fs::read(path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&data) else {
        return Vec::new();
    };
    value
        .get("tokens")
        .and_then(|tokens| tokens.as_array())
        .map(|tokens| {
            tokens
                .iter()
                .filter_map(|token| token.as_str())
                .filter(|token| !token.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The token file is a credential store; keep it off other accounts.
#[cfg(unix)]
fn restrict(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fqapi-tokens-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn bootstrap_tokens_are_never_removable() {
        let dir = temp_dir("bootstrap");
        let store = TokenStore::load(&dir, vec!["boot-token".to_string()]);
        assert!(store.contains("boot-token"));
        assert!(store.remove("boot-token").is_err());
        assert!(store.contains("boot-token"), "the refusal must not drop it");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn added_tokens_survive_a_reload() {
        let dir = temp_dir("persist");
        {
            let store = TokenStore::load(&dir, vec!["boot".to_string()]);
            store.add("runtime-token-1").expect("add");
        }
        let reloaded = TokenStore::load(&dir, vec!["boot".to_string()]);
        assert!(reloaded.contains("runtime-token-1"));
        assert_eq!(reloaded.list().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn removal_is_persisted_too() {
        let dir = temp_dir("remove");
        {
            let store = TokenStore::load(&dir, Vec::new());
            store.add("runtime-token-1").expect("add");
            store.remove("runtime-token-1").expect("remove");
        }
        let reloaded = TokenStore::load(&dir, Vec::new());
        assert!(!reloaded.contains("runtime-token-1"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn short_and_duplicate_tokens_are_rejected() {
        let dir = temp_dir("reject");
        let store = TokenStore::load(&dir, vec!["boot-token".to_string()]);
        assert!(store.add("short").is_err());
        assert!(
            store.add("boot-token").is_err(),
            "already a bootstrap token"
        );
        store.add("runtime-token-1").expect("first");
        assert!(store.add("runtime-token-1").is_err(), "duplicate");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_file_reads_as_empty_instead_of_failing_startup() {
        let dir = temp_dir("corrupt");
        std::fs::write(dir.join(FILE_NAME), b"{not json").expect("seed");
        let store = TokenStore::load(&dir, vec!["boot".to_string()]);
        assert!(store.contains("boot"), "bootstrap still works");
        assert_eq!(store.list().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
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
