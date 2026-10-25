//! Device-pool encryption key.
//!
//! The core encrypts `device_pool.json` when a key is provisioned, and stores
//! plaintext with a warning when it is not. On Android the key came from the
//! platform keystore; a server has no such store, so the key is operator-owned
//! material resolved in this order:
//!
//! 1. `FQ_POOL_KEY` (64 hex characters)
//! 2. `<data-dir>/pool.key` (64 hex characters, created on first start)
//! 3. generated and written to `<data-dir>/pool.key`
//!
//! The file is text rather than 32 raw bytes so it can be copied between hosts
//! or pasted into `FQ_POOL_KEY` without an encoding step.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use fqapi_core::device::pool_crypto;

const KEY_LEN: usize = 32;
pub const KEY_FILE_NAME: &str = "pool.key";

/// Resolves the pool key, creating one only when that cannot destroy state.
///
/// `env_key` is passed in rather than read here so tests do not have to mutate
/// process-wide environment variables.
pub fn load_or_create(
    data_dir: &Path,
    pool_path: &Path,
    env_key: Option<String>,
) -> Result<[u8; KEY_LEN], String> {
    if let Some(raw) = env_key {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err("FQ_POOL_KEY is set but empty".to_string());
        }
        return parse_hex(raw).map_err(|e| format!("FQ_POOL_KEY: {e}"));
    }

    let key_path = data_dir.join(KEY_FILE_NAME);
    if key_path.exists() {
        let text = std::fs::read_to_string(&key_path)
            .map_err(|e| format!("read {}: {e}", key_path.display()))?;
        return parse_hex(text.trim()).map_err(|e| format!("{}: {e}", key_path.display()));
    }

    // Refuse to mint a key over an existing encrypted pool: the new key would
    // not open it, the pool would load as empty, and every device identity in
    // it would be replaced by freshly registered ones.
    if pool_file_is_encrypted(pool_path) {
        return Err(format!(
            "{} is encrypted but no key is available. Restore pool.key or set \
             FQ_POOL_KEY; generating a new key here would discard every device \
             identity in the pool.",
            pool_path.display()
        ));
    }

    let key = generate();
    write_key(&key_path, &key)?;
    Ok(key)
}

fn pool_file_is_encrypted(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = [0u8; 7];
    match file.read_exact(&mut head) {
        // Reuse the core's own magic check instead of repeating "FQPOOL1".
        Ok(()) => pool_crypto::pool_file_is_encrypted(&head),
        Err(_) => false,
    }
}

/// Whether an existing pool file is still plaintext and therefore needs a save
/// now that a key is provisioned. A missing file is not "plaintext": there is
/// nothing to migrate and writing one would create an empty pool.
pub fn needs_migration(pool_path: &Path) -> bool {
    pool_path.exists() && !pool_file_is_encrypted(pool_path)
}

fn generate() -> [u8; KEY_LEN] {
    use rand::RngCore;
    let mut key = [0u8; KEY_LEN];
    rand::rngs::OsRng.fill_bytes(&mut key);
    key
}

fn write_key(path: &Path, key: &[u8; KEY_LEN]) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    // Create with the final mode rather than chmod-ing afterwards, so the key
    // is never briefly world-readable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    match options.open(path) {
        Ok(mut file) => {
            let text = hex::encode(key);
            file.write_all(text.as_bytes())
                .and_then(|()| file.write_all(b"\n"))
                .map_err(|e| format!("write {}: {e}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(format!(
            "{} appeared while starting; another instance is using this data directory",
            path.display()
        )),
        Err(e) => Err(format!("create {}: {e}", path.display())),
    }
}

fn parse_hex(raw: &str) -> Result<[u8; KEY_LEN], String> {
    let bytes = hex::decode(raw).map_err(|e| format!("not hex: {e}"))?;
    if bytes.len() != KEY_LEN {
        return Err(format!(
            "expected {KEY_LEN} bytes ({} hex characters), got {} bytes",
            KEY_LEN * 2,
            bytes.len()
        ));
    }
    let mut key = [0u8; KEY_LEN];
    key.copy_from_slice(&bytes);
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Unique per call rather than per thread: the test harness may reuse a
    /// worker thread, and a shared directory would let one test's cleanup
    /// delete another's files.
    static DIR_SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(tag: &str) -> PathBuf {
        let n = DIR_SEQ.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "fqapi-server-poolkey-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn generates_and_persists_a_key_on_first_start() {
        let dir = temp_dir("create");
        let pool = dir.join("device_pool.json");
        let key = load_or_create(&dir, &pool, None).expect("generate");

        let path = dir.join(KEY_FILE_NAME);
        assert!(path.exists(), "the key must be persisted");
        let text = std::fs::read_to_string(&path).expect("read");
        assert_eq!(text.trim().len(), KEY_LEN * 2, "64 hex characters");
        assert_eq!(hex::decode(text.trim()).expect("hex"), key.to_vec());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reuses_the_persisted_key() {
        let dir = temp_dir("reuse");
        let pool = dir.join("device_pool.json");
        let first = load_or_create(&dir, &pool, None).expect("first");
        let second = load_or_create(&dir, &pool, None).expect("second");
        assert_eq!(first, second, "a restart must not rotate the pool key");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_environment_key_wins_over_the_file() {
        let dir = temp_dir("env");
        let pool = dir.join("device_pool.json");
        let _ = load_or_create(&dir, &pool, None).expect("seed file");

        let env_key = "11".repeat(KEY_LEN);
        let key = load_or_create(&dir, &pool, Some(env_key.clone())).expect("env");
        assert_eq!(hex::encode(key), env_key);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_keys_are_rejected_rather_than_truncated() {
        let dir = temp_dir("bad");
        let pool = dir.join("device_pool.json");
        assert!(load_or_create(&dir, &pool, Some("not hex".to_string())).is_err());
        assert!(load_or_create(&dir, &pool, Some("11".repeat(16))).is_err());
        assert!(load_or_create(&dir, &pool, Some(String::new())).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_to_mint_a_key_over_an_encrypted_pool() {
        let dir = temp_dir("encrypted");
        let pool = dir.join("device_pool.json");
        // The core's on-disk header, followed by bytes that do not matter here.
        std::fs::write(&pool, b"FQPOOL1\x00\x00\x00\x00").expect("seed");

        let err = load_or_create(&dir, &pool, None).expect_err("must refuse");
        assert!(
            err.contains("encrypted"),
            "the message must say why: {err}"
        );
        assert!(
            !dir.join(KEY_FILE_NAME).exists(),
            "no key file may be written when refusing"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_plaintext_pool_does_not_block_key_generation() {
        let dir = temp_dir("plaintext");
        let pool = dir.join("device_pool.json");
        std::fs::write(&pool, br#"{"android":[],"last_update":""}"#).expect("seed");
        assert!(load_or_create(&dir, &pool, None).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn migration_is_needed_only_for_an_existing_plaintext_pool() {
        let dir = temp_dir("migrate");
        let pool = dir.join("device_pool.json");
        assert!(
            !needs_migration(&pool),
            "a missing pool has nothing to migrate, and writing one would create an empty pool"
        );

        std::fs::write(&pool, br#"{"android":[]}"#).expect("plaintext");
        assert!(needs_migration(&pool));

        std::fs::write(&pool, b"FQPOOL1\x00\x00").expect("encrypted");
        assert!(!needs_migration(&pool), "an encrypted pool is already migrated");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn the_key_file_is_not_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("mode");
        let pool = dir.join("device_pool.json");
        load_or_create(&dir, &pool, None).expect("generate");
        let mode = std::fs::metadata(dir.join(KEY_FILE_NAME))
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "group and other must have no access");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
