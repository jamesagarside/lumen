//! Encrypted-at-rest secret store for admin-managed configuration.
//!
//! Used by `settings` to hold passwords, API keys, and webhook URLs
//! that the admin UI lets the operator manage at runtime. The plain
//! settings KV (URLs, site names, intervals) lives unencrypted in
//! the same redb file; this module handles the things we'd be
//! embarrassed to dump to a log.
//!
//! ## Threat model
//!
//! In scope: protect secrets from anyone who walks off with `data/`
//! but not `data/master.key`. That's the common backup/disk-snapshot
//! mistake — if backups exclude the master key, the secrets they
//! contain are useless to an attacker.
//!
//! Out of scope: defending against a root attacker on the live host.
//! If they can read `master.key` they can also read the daemon's
//! memory; envelope encryption doesn't help there.
//!
//! ## Crypto
//!
//! ChaCha20-Poly1305 AEAD with a 32-byte key and 12-byte random
//! nonces. Each ciphertext is a versioned envelope:
//!
//!   `[version: 1 byte][nonce: 12 bytes][ciphertext+tag: variable]`
//!
//! `version = 0` is the only one defined today. A future key-rotation
//! scheme would bump the version and carry a `key_id` field in the
//! envelope; for v1 there's one master key.
//!
//! ## Master key lifecycle
//!
//! - `LUMEN_MASTER_KEY` (hex-encoded 32 bytes) wins if set. Useful for
//!   container deployments where the key lives in a secrets manager.
//! - Otherwise a `master.key` file in the data dir is used. Generated
//!   on first run with mode `0600`. The daemon logs *loudly* the first
//!   time it generates one — losing the file means losing every secret.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use rand::RngCore;
use redb::{Database, TableDefinition};
use tracing::{info, warn};

const SECRETS: TableDefinition<&str, &[u8]> = TableDefinition::new("secrets");

const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;
const ENVELOPE_VERSION: u8 = 0;

const MASTER_KEY_ENV: &str = "LUMEN_MASTER_KEY";
const MASTER_KEY_FILE: &str = "master.key";

/// Cheap-to-clone handle. Owns the cipher (which is itself cheap to
/// clone) and shares the redb database with the rest of the daemon.
#[derive(Clone)]
pub struct SecretStore {
    db: Arc<Database>,
    cipher: ChaCha20Poly1305,
}

impl SecretStore {
    /// Open the store, loading the master key from env or file.
    /// Creates the file (and generates a new key) on first run.
    pub fn open(db: Arc<Database>, data_dir: &Path) -> Result<Self> {
        let key = load_or_create_master_key(data_dir)?;
        Self::with_key(db, &key)
    }

    /// Construct with an explicit 32-byte key. Used by tests and by
    /// the env-var path; production code should call `open`.
    pub fn with_key(db: Arc<Database>, key_bytes: &[u8; KEY_LEN]) -> Result<Self> {
        // Ensure the table exists so reads on a fresh DB don't error.
        let txn = db.begin_write().context("secrets: open write txn")?;
        {
            txn.open_table(SECRETS)
                .context("secrets: open secrets table")?;
        }
        txn.commit().context("secrets: commit init")?;
        let cipher = ChaCha20Poly1305::new(Key::from_slice(key_bytes));
        Ok(Self { db, cipher })
    }

    /// Store or replace a secret. Empty value clears the entry — the
    /// admin UI maps "leave blank" to that.
    pub fn put(&self, name: &str, value: &str) -> Result<()> {
        if value.is_empty() {
            return self.delete(name);
        }
        let envelope = self.seal(value.as_bytes())?;
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(SECRETS)?;
            table.insert(name, envelope.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }

    /// Fetch and decrypt. Returns `Ok(None)` if missing. Decryption
    /// failure (wrong key, corrupt envelope) is an error — silent
    /// "looks unset" would hide a real configuration problem.
    pub fn get(&self, name: &str) -> Result<Option<String>> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(SECRETS)?;
        let Some(value) = table.get(name)? else {
            return Ok(None);
        };
        let bytes = value.value().to_vec();
        drop(table);
        drop(txn);
        let plaintext = self
            .open_envelope(&bytes)
            .with_context(|| format!("secrets: decrypt {name}"))?;
        Ok(Some(
            String::from_utf8(plaintext).context("secrets: stored value is not UTF-8")?,
        ))
    }

    /// True if a value is currently stored for this name. Does not
    /// decrypt — useful for the redacted GET that the admin API
    /// returns ("configured" without leaking the value).
    pub fn has(&self, name: &str) -> Result<bool> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(SECRETS)?;
        Ok(table.get(name)?.is_some())
    }

    pub fn delete(&self, name: &str) -> Result<()> {
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(SECRETS)?;
            table.remove(name)?;
        }
        txn.commit()?;
        Ok(())
    }

    fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = self
            .cipher
            .encrypt(nonce, plaintext)
            .map_err(|e| anyhow!("secrets: encrypt failed: {e}"))?;
        let mut envelope = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
        envelope.push(ENVELOPE_VERSION);
        envelope.extend_from_slice(&nonce_bytes);
        envelope.extend_from_slice(&ciphertext);
        Ok(envelope)
    }

    fn open_envelope(&self, envelope: &[u8]) -> Result<Vec<u8>> {
        if envelope.len() < 1 + NONCE_LEN {
            return Err(anyhow!("envelope too short"));
        }
        let version = envelope[0];
        if version != ENVELOPE_VERSION {
            return Err(anyhow!(
                "unsupported envelope version {version} (this build only knows v{ENVELOPE_VERSION})"
            ));
        }
        let nonce = Nonce::from_slice(&envelope[1..1 + NONCE_LEN]);
        let ciphertext = &envelope[1 + NONCE_LEN..];
        self.cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| anyhow!("decrypt failed (wrong master key?): {e}"))
    }
}

/// Load the master key from the env var if present, otherwise from
/// the `master.key` file in `data_dir`. Generates the file (with a
/// loud one-time warning) on first run.
fn load_or_create_master_key(data_dir: &Path) -> Result<[u8; KEY_LEN]> {
    if let Ok(hex_value) = std::env::var(MASTER_KEY_ENV) {
        let bytes = hex::decode(hex_value.trim())
            .with_context(|| format!("{MASTER_KEY_ENV} must be hex-encoded"))?;
        if bytes.len() != KEY_LEN {
            return Err(anyhow!(
                "{MASTER_KEY_ENV} must be exactly {KEY_LEN} bytes ({} hex chars); got {} bytes",
                KEY_LEN * 2,
                bytes.len()
            ));
        }
        info!(
            "secret store: master key loaded from {} env var",
            MASTER_KEY_ENV
        );
        let mut key = [0u8; KEY_LEN];
        key.copy_from_slice(&bytes);
        return Ok(key);
    }

    let path = data_dir.join(MASTER_KEY_FILE);
    if path.exists() {
        let hex_value = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let bytes = hex::decode(hex_value.trim()).with_context(|| {
            format!("{} is not valid hex (delete and restart to regenerate, but you'll lose any stored secrets)", path.display())
        })?;
        if bytes.len() != KEY_LEN {
            return Err(anyhow!(
                "{} must be exactly {KEY_LEN} bytes; got {}",
                path.display(),
                bytes.len()
            ));
        }
        let mut key = [0u8; KEY_LEN];
        key.copy_from_slice(&bytes);
        return Ok(key);
    }

    // First-run path. Generate, persist with 0600, log loudly.
    let mut key = [0u8; KEY_LEN];
    rand::thread_rng().fill_bytes(&mut key);
    write_key_file(&path, &key)?;
    warn!(
        path = %path.display(),
        "secret store: generated new master key — back this file up alongside your data dir. \
         Losing it means losing every admin-managed secret. Set {} to take it out of band.",
        MASTER_KEY_ENV
    );
    Ok(key)
}

#[cfg(unix)]
fn write_key_file(path: &Path, key: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    f.write_all(hex::encode(key).as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn write_key_file(path: &Path, key: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut f =
        std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
    f.write_all(hex::encode(key).as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Re-exported path for callers that want to log where the key lives.
#[allow(dead_code)]
pub fn master_key_path(data_dir: &Path) -> PathBuf {
    data_dir.join(MASTER_KEY_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use redb::ReadableTable;
    use tempfile::tempdir;

    fn open_store() -> (tempfile::TempDir, SecretStore, [u8; KEY_LEN]) {
        let dir = tempdir().unwrap();
        let db = Arc::new(Database::create(dir.path().join("t.redb")).unwrap());
        let mut key = [0u8; KEY_LEN];
        rand::thread_rng().fill_bytes(&mut key);
        let store = SecretStore::with_key(db, &key).unwrap();
        (dir, store, key)
    }

    #[test]
    fn round_trip_string() {
        let (_dir, store, _) = open_store();
        store.put("UDM_PASSWORD", "hunter2!").unwrap();
        assert_eq!(
            store.get("UDM_PASSWORD").unwrap().as_deref(),
            Some("hunter2!")
        );
    }

    #[test]
    fn round_trip_unicode_and_long_values() {
        let (_dir, store, _) = open_store();
        let value = "日本語🔑with some long padding ".repeat(50);
        store.put("k", &value).unwrap();
        assert_eq!(store.get("k").unwrap().as_deref(), Some(value.as_str()));
    }

    #[test]
    fn missing_secret_returns_none() {
        let (_dir, store, _) = open_store();
        assert!(store.get("nope").unwrap().is_none());
        assert!(!store.has("nope").unwrap());
    }

    #[test]
    fn put_empty_value_clears_entry() {
        let (_dir, store, _) = open_store();
        store.put("k", "v").unwrap();
        assert!(store.has("k").unwrap());
        store.put("k", "").unwrap();
        assert!(!store.has("k").unwrap());
    }

    #[test]
    fn delete_removes_entry() {
        let (_dir, store, _) = open_store();
        store.put("k", "v").unwrap();
        store.delete("k").unwrap();
        assert!(store.get("k").unwrap().is_none());
    }

    #[test]
    fn has_does_not_decrypt() {
        let (_dir, store, _) = open_store();
        store.put("k", "v").unwrap();
        // has() should answer even without the right key.
        let dir = tempdir().unwrap();
        let db = Arc::new(Database::create(dir.path().join("other.redb")).unwrap());
        let wrong_store = SecretStore::with_key(db, &[7u8; KEY_LEN]).unwrap();
        wrong_store.put("k", "v").unwrap();
        assert!(wrong_store.has("k").unwrap());
    }

    #[test]
    fn wrong_key_fails_to_decrypt() {
        let dir = tempdir().unwrap();
        let db = Arc::new(Database::create(dir.path().join("t.redb")).unwrap());
        let store_a = SecretStore::with_key(db.clone(), &[1u8; KEY_LEN]).unwrap();
        store_a.put("k", "secret").unwrap();
        let store_b = SecretStore::with_key(db, &[2u8; KEY_LEN]).unwrap();
        let err = store_b.get("k").unwrap_err();
        assert!(
            format!("{err:#}").contains("decrypt"),
            "expected decrypt failure, got {err:#}"
        );
    }

    #[test]
    fn same_plaintext_yields_different_ciphertext() {
        // Ensures we're not using a constant nonce by accident — a
        // failure mode that's catastrophic for AEAD modes.
        let (_dir, store, _) = open_store();
        store.put("k1", "same").unwrap();
        store.put("k2", "same").unwrap();
        let txn = store.db.begin_read().unwrap();
        let table = txn.open_table(SECRETS).unwrap();
        let c1 = table.get("k1").unwrap().unwrap().value().to_vec();
        let c2 = table.get("k2").unwrap().unwrap().value().to_vec();
        assert_ne!(c1, c2, "nonce must be random per-encryption");
    }

    #[test]
    fn unsupported_envelope_version_is_rejected() {
        let (_dir, store, _) = open_store();
        store.put("k", "v").unwrap();
        // Poke a bad version byte into the stored envelope.
        let txn = store.db.begin_write().unwrap();
        {
            let mut table = txn.open_table(SECRETS).unwrap();
            let bytes = table.get("k").unwrap().unwrap().value().to_vec();
            let mut tampered = bytes.clone();
            tampered[0] = 99;
            table.insert("k", tampered.as_slice()).unwrap();
        }
        txn.commit().unwrap();
        let err = store.get("k").unwrap_err();
        assert!(format!("{err:#}").contains("envelope version"));
    }

    #[test]
    fn master_key_file_is_created_with_0600_on_first_run() {
        let dir = tempdir().unwrap();
        let _key = load_or_create_master_key(dir.path()).unwrap();
        let path = dir.path().join(MASTER_KEY_FILE);
        assert!(path.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode, 0o600,
                "master.key permissions should be 0600, got {mode:o}"
            );
        }
    }

    #[test]
    fn master_key_file_reuses_on_subsequent_runs() {
        let dir = tempdir().unwrap();
        let a = load_or_create_master_key(dir.path()).unwrap();
        let b = load_or_create_master_key(dir.path()).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn master_key_env_overrides_file() {
        let dir = tempdir().unwrap();
        // Pre-create a file key.
        let file_key = load_or_create_master_key(dir.path()).unwrap();
        let env_key = [42u8; KEY_LEN];
        // Set + clear the env var around the test (no other test
        // reads this var so serial isolation isn't critical, but we
        // still clean up).
        // SAFETY: tests don't run multi-threaded by default within a
        // single #[test], and other tests don't touch this var.
        unsafe {
            std::env::set_var(MASTER_KEY_ENV, hex::encode(env_key));
        }
        let loaded = load_or_create_master_key(dir.path()).unwrap();
        unsafe {
            std::env::remove_var(MASTER_KEY_ENV);
        }
        assert_eq!(loaded, env_key);
        assert_ne!(loaded, file_key);
    }

    #[test]
    fn master_key_env_rejects_wrong_length() {
        let dir = tempdir().unwrap();
        unsafe {
            std::env::set_var(MASTER_KEY_ENV, "deadbeef");
        }
        let err = load_or_create_master_key(dir.path()).unwrap_err();
        unsafe {
            std::env::remove_var(MASTER_KEY_ENV);
        }
        assert!(format!("{err:#}").contains("32 bytes"));
    }
}
